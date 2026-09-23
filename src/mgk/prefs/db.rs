use crate::config::Settings;
use crate::kv::KvStore;
use crate::mgk::{CreatePreference, GetAddress};
use anyhow::{Context, Result};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use typed_eventbus::{Event, EventStream, EventType};
use validator::Validate;
use viewset::{Entity, Repository};

fn gen_otp() -> u32 {
    let mut rng = rand::rng();
    rng.random_range(100000..999999)
}

/// Returns an alphanumeric nonce used as the pending-confirmation lookup key.
/// This is separate from the OTP so that the user-facing 6-digit code
/// carries no entropy about which slot to attack.
fn gen_nonce() -> String {
    let mut rng = rand::rng();
    (0..16)
        .map(|_| {
            let idx: u8 = rng.random_range(0..36);
            if idx < 10 {
                (b'0' + idx) as char
            } else {
                (b'a' + idx - 10) as char
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

/// A single (subject, address) pair supplied by the client.
#[derive(Deserialize, Validate, Clone, Serialize)]
pub struct Preference {
    #[validate(length(min = 1, max = 64))]
    pub subject: String,
    #[validate(length(min = 1, max = 64))]
    pub address: String,
}

/// A batch of preferences that all share the same address.
/// One OTP is generated for the batch; confirming it writes every row.
#[derive(Deserialize, Validate)]
pub struct PreferenceBatch {
    #[validate(length(min = 1))]
    #[validate(nested)]
    pub preferences: Vec<Preference>,
}

#[derive(Deserialize, Validate)]
pub struct Token {
    #[validate(range(min = 100000, max = 999999))]
    pub token: u32,
}

// ---------------------------------------------------------------------------
// Pending entry
// ---------------------------------------------------------------------------

/// What we keep in Redis while waiting for OTP confirmation (JSON, with a TTL).
/// All preferences in a batch share a single address, which is validated
/// to be identical across entries before the batch is accepted.
#[derive(Serialize, Deserialize)]
struct PendingEntry {
    otp: u32,
    /// `(subject, address)` pairs — address is repeated per row so that
    /// `confirm` can write each row independently without extra state.
    items: Vec<(String, String)>,
}

// ---------------------------------------------------------------------------
// Preferences
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Preferences<Repo: Repository> {
    db: Arc<Repo>,
    /// Shared (Redis) store: pending OTPs and the (user, subject) -> address cache.
    kv: Arc<dyn KvStore>,
    allowed_subjects: HashSet<String>,
    /// Channel name derived from the entity table ("email_preferences" -> "email").
    channel: String,
    settings: Settings,
    es: Arc<dyn EventStream>,
}

impl<Repo: Repository> Preferences<Repo>
where
    <<Repo as Repository>::Entity as Entity>::CreateDto: From<CreatePreference>,
    <Repo as Repository>::Entity: GetAddress,
{
    pub async fn new(
        db: Arc<Repo>,
        es: Arc<dyn EventStream>,
        subjects: Vec<String>,
        kv: Arc<dyn KvStore>,
        settings: Settings,
    ) -> Result<Self> {
        let table = <<Repo as Repository>::Entity as Entity>::TABLE;
        let channel = table
            .strip_suffix("_preferences")
            .unwrap_or(table)
            .to_string();
        Ok(Self {
            db,
            es,
            kv,
            channel,
            settings,
            allowed_subjects: subjects.into_iter().collect(),
        })
    }

    fn pending_key(&self, user: &str, nonce: &str) -> String {
        format!("pending:{}:{}:{}", self.channel, user, nonce)
    }

    fn cache_key(&self, user: &str, subject: &str) -> String {
        format!("pref:{}:{}:{}", self.channel, user, subject)
    }

    // -----------------------------------------------------------------------
    // set — accepts a batch of preferences, returns (nonce, otp)
    //
    // The nonce is an opaque handle stored server-side; the OTP is the
    // 6-digit code sent out-of-band to the user.  The handler sends the OTP
    // via the Sender and returns the nonce in the HTTP response so the client
    // can pair them on /confirm.
    // -----------------------------------------------------------------------
    pub async fn set(&self, user: &str, batch: PreferenceBatch) -> Result<(String, u32)> {
        if let Err(e) = batch.validate() {
            return Err(anyhow::anyhow!("Invalid data: {e}"));
        }

        // All preferences must share the same address.
        let address = &batch.preferences[0].address;
        for pref in &batch.preferences {
            if &pref.address != address {
                return Err(anyhow::anyhow!(
                    "All preferences in a batch must share the same address"
                ));
            }
            if !self.allowed_subjects.contains(&pref.subject) {
                return Err(anyhow::anyhow!("Subject not allowed: {}", pref.subject));
            }
        }

        let otp = gen_otp();
        let nonce = gen_nonce();

        let items = batch
            .preferences
            .into_iter()
            .map(|p| (p.subject, p.address))
            .collect();

        let json = serde_json::to_string(&PendingEntry { otp, items })?;
        self.kv
            .set(
                &self.pending_key(user, &nonce),
                &json,
                self.settings.otp_ttl,
            )
            .await
            .context("could not store pending confirmation")?;

        Ok((nonce, otp))
    }

    // -----------------------------------------------------------------------
    // confirm — validates OTP against the nonce, writes all rows
    // -----------------------------------------------------------------------
    pub async fn confirm(&self, user: &str, nonce: &str, otp: &Token) -> Result<()> {
        if let Err(e) = otp.validate() {
            return Err(anyhow::anyhow!("invalid token: {e}"));
        }

        let key = self.pending_key(user, nonce);
        let raw = self
            .kv
            .get(&key)
            .await
            .context("could not read pending confirmation")?
            .ok_or_else(|| anyhow::anyhow!("Token not found or expired"))?;
        let entry: PendingEntry =
            serde_json::from_str(&raw).context("corrupt pending confirmation")?;

        if entry.otp != otp.token {
            return Err(anyhow::anyhow!("Token not found or expired"));
        }

        // Consume the entry now that it has been verified.
        self.kv
            .delete(&key)
            .await
            .context("could not consume pending confirmation")?;

        // (user_id, subject) is unique, so setting an address again replaces the
        // previous one rather than adding a second row.
        let upsert = format!(
            "INSERT INTO {} (subject, address, user_id) VALUES ($1, $2, $3) \
             ON CONFLICT (user_id, subject) DO UPDATE SET address = EXCLUDED.address",
            <<Repo as Repository>::Entity as Entity>::TABLE
        );

        for (subject, address) in &entry.items {
            sqlx::query(sqlx::AssertSqlSafe(upsert.clone()))
                .bind(subject.as_str())
                .bind(address.as_str())
                .bind(user)
                .execute(self.db.database())
                .await?;

            if let Err(e) = self
                .kv
                .set(
                    &self.cache_key(user, subject),
                    address,
                    self.settings.cache_ttl,
                )
                .await
            {
                tracing::warn!(error = %e, user, subject, "Failed to refresh preference cache");
            }

            let event = ChannelConfirmed {
                user: user.to_string(),
                channel: self.channel.clone(),
                address: address.clone(),
            };

            let ev = Event::new(event).with_producer("mgk");
            // Best-effort publish; a failure here must not roll back the DB write.
            if let Err(e) = ev.publish(self.es.clone()).await {
                tracing::warn!(error = %e, user, subject, "Failed to publish ChannelConfirmed event");
            }
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // get — cache-aside read (Redis, falling back to the database)
    // -----------------------------------------------------------------------
    pub async fn get(&self, user: &str, subject: &str) -> Result<Option<String>> {
        let key = self.cache_key(user, subject);

        match self.kv.get(&key).await {
            Ok(Some(cached)) => return Ok(Some(cached)),
            Ok(None) => {}
            // A cache outage must not stop delivery: fall through to the database.
            Err(e) => tracing::warn!(error = %e, "Preference cache read failed, using database"),
        }

        let filters: HashMap<&str, String> = vec![
            ("user_id", user.to_string()),
            ("subject", subject.to_string()),
        ]
        .into_iter()
        .collect();
        let (rows, _count) = self.db.list(&filters.into()).await?;
        if let Some(first) = rows.first() {
            let address = first.get_address();
            if let Err(e) = self.kv.set(&key, &address, self.settings.cache_ttl).await {
                tracing::warn!(error = %e, "Failed to populate preference cache");
            }
            return Ok(Some(address));
        }
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct ChannelConfirmed {
    user: String,
    channel: String,
    address: String,
}

impl EventType for ChannelConfirmed {
    const SUBJECT: &'static str = "contact.channel.confirmed";
}
