use crate::mgk::{CreatePreference, GetAddress};
use anyhow::Result;
use moka::future::Cache;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::{collections::HashSet, sync::Arc, time::Duration};
use typed_eventbus::{Event, EventStream, Publishable};
use validator::Validate;
use viewset::{Entity, Repository};

fn gen_otp() -> u32 {
    let mut rng = rand::rng();
    rng.random_range(100000..999999)
}

/// Returns an alphanumeric nonce used as the pending-cache lookup key.
/// This is separate from the OTP so that the user-facing 6-digit code
/// carries no entropy about which cache slot to attack.
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

/// What we store in the pending cache while waiting for OTP confirmation.
/// All preferences in a batch share a single address, which is validated
/// to be identical across entries before the batch is accepted.
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
    /// (user, subject) -> address
    cache: Cache<(String, String), String>,
    /// nonce -> PendingEntry  (nonce is returned to the handler, not the user)
    pending: Cache<String, Arc<PendingEntry>>,
    allowed_subjects: HashSet<String>,
    table_name: String,
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
    ) -> Result<Self> {
        let table_name = format!(
            "{}_preferences",
            <<Repo as Repository>::Entity as Entity>::TABLE
        );
        Ok(Self {
            db,
            es,
            table_name,
            cache: Cache::builder().max_capacity(1000).build(),
            pending: Cache::builder()
                .max_capacity(100)
                // OTP tokens expire after 10 minutes.
                .time_to_live(Duration::from_secs(300))
                .build(),
            allowed_subjects: subjects.into_iter().collect(),
        })
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

        self.pending
            .insert(
                format!("{}:{}", user, nonce),
                Arc::new(PendingEntry { otp, items }),
            )
            .await;

        Ok((nonce, otp))
    }

    // -----------------------------------------------------------------------
    // confirm — validates OTP against the nonce, writes all rows
    // -----------------------------------------------------------------------
    pub async fn confirm(&self, user: &str, nonce: &str, otp: &Token) -> Result<()> {
        if let Err(e) = otp.validate() {
            return Err(anyhow::anyhow!("invalid token: {e}"));
        }

        let key = format!("{}:{}", user, nonce);
        let entry = match self.pending.get(&key).await {
            Some(e) => e,
            None => return Err(anyhow::anyhow!("Token not found or expired")),
        };

        if entry.otp != otp.token {
            return Err(anyhow::anyhow!("Token not found or expired"));
        }

        // Remove the entry now that it has been consumed.
        self.pending.remove(&key).await;

        for (subject, address) in &entry.items {
            let pref = CreatePreference::new(user.into(), subject.into(), address.into());
            self.db.create(pref.into()).await?;

            self.cache
                .insert((user.to_string(), subject.clone()), address.clone())
                .await;

            let event = ChannelConfirmed {
                user: user.to_string(),
                channel: self.table_name.replace("_preferences", ""),
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
    // get — cache-aside read
    // -----------------------------------------------------------------------
    pub async fn get(&self, user: &str, subject: &str) -> Result<Option<String>> {
        let key = (user.to_string(), subject.to_string());

        if let Some(cached) = self.cache.get(&key).await {
            return Ok(Some(cached));
        }

        /*let result = sqlx::query_scalar::<_, String>(&format!(
            "SELECT address FROM {} WHERE user = ? AND subject = ?",
            self.table_name
        ))
        .bind(user)
        .bind(subject)
        .fetch_optional(&self.db)*/
        let filters: HashMap<&str, String> =
            vec![("user", user.to_string()), ("subject", subject.to_string())]
                .into_iter()
                .collect();
        let result = self.db.list(&filters.into()).await?;
        let (addresses, _count) = result;
        if addresses.len() > 0 {
            let address = addresses[0].get_address();
            self.cache.insert(key, address.clone()).await;
            return Ok(Some(address));
        }
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ChannelConfirmed {
    user: String,
    channel: String,
    address: String,
}

impl Publishable for ChannelConfirmed {
    const SUBJECT: &'static str = "contact.channel.confirmed";
}
