use crate::config::Settings;
use crate::mgk::{CreatePreference, GetAddress};
use ferrumec::Store;
use ferrumec::cache::CacheFactory;
use ferrumec::event::{Event, EventStream, EventType};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use validator::Validate;
use viewset::{Entity, Repository};

pub type Result<T> = core::result::Result<T, Box<dyn std::error::Error>>;

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
#[derive(Serialize, Deserialize, Clone)]
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
    kv: Arc<dyn Store<(String, String), PendingEntry>>,
    allowed_subjects: HashSet<String>,
    /// Channel name derived from the entity table ("email_preferences" -> "email").
    channel: String,
    settings: Settings,
    /// (user, subject) -> address. Read on every notification delivery.
    addresses: Arc<dyn Store<(String, String), String>>,
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
        cf: impl CacheFactory,
        settings: Settings,
    ) -> Result<Self> {
        let table = <<Repo as Repository>::Entity as Entity>::TABLE;
        let channel = table
            .strip_suffix("_preferences")
            .unwrap_or(table)
            .to_string();
        let kv = cf.new_cache(&channel, Duration::from_mins(30));
        let addresses = cf.new_cache(&format!("{channel}_addresses"), settings.cache_ttl);
        Ok(Self {
            db,
            es,
            kv,
            addresses,
            channel,
            settings,
            allowed_subjects: subjects.into_iter().collect(),
        })
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
            return Err(anyhow::anyhow!("Invalid data: {e}").into());
        }

        // All preferences must share the same address.
        let address = &batch.preferences[0].address;
        for pref in &batch.preferences {
            if &pref.address != address {
                return Err(anyhow::anyhow!(
                    "All preferences in a batch must share the same address"
                )
                .into());
            }
            if !self.allowed_subjects.contains(&pref.subject) {
                return Err(anyhow::anyhow!("Subject not allowed: {}", pref.subject).into());
            }
        }

        let otp = gen_otp();
        let nonce = gen_nonce();

        let items = batch
            .preferences
            .into_iter()
            .map(|p| (p.subject, p.address))
            .collect();

        let json = PendingEntry { otp, items };
        self.kv
            .set(&(user.to_string(), nonce.clone()), json)
            .await?;
        Ok((nonce, otp))
    }

    // -----------------------------------------------------------------------
    // confirm — validates OTP against the nonce, writes all rows
    // -----------------------------------------------------------------------
    pub async fn confirm(&self, user: &str, nonce: &str, otp: &Token) -> Result<()> {
        if let Err(e) = otp.validate() {
            return Err(anyhow::anyhow!("invalid token: {e}").into());
        }

        let key = (user.to_string(), nonce.to_string());
        let entry = match self.kv.get(&key).await? {
            Some(r) => r,
            None => return Err(anyhow::anyhow!("no pending entry").into()),
        };

        if entry.otp != otp.token {
            return Err(anyhow::anyhow!("Token not found or expired").into());
        }

        // Consume the entry now that it has been verified.
        self.kv.delete(&key).await?;

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

            let akey = (user.to_string(), subject.clone());
            if let Err(e) = self.addresses.set(&akey, address.clone()).await {
                tracing::warn!(error = %e, "failed to update address cache");
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
        let key = (user.to_string(), subject.to_string());
        if let Ok(Some(hit)) = self.addresses.get(&key).await {
            return Ok(Some(hit));
        }
        let sql = format!(
            "SELECT address FROM {} WHERE user_id = $1 AND subject = $2 LIMIT 1",
            <<Repo as Repository>::Entity as Entity>::TABLE
        );
        let address = sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(sql))
            .bind(user)
            .bind(subject)
            .fetch_optional(self.db.database())
            .await?;
        if let Some(a) = &address {
            if let Err(e) = self.addresses.set(&key, a.clone()).await {
                tracing::warn!(error = %e, "failed to populate address cache");
            }
        }
        Ok(address)
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
