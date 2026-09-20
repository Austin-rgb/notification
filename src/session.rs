//! Cookie-based session identity for the notification service.
//!
//! Sessions are created by an upstream auth service and stored under a UUID
//! cookie. This service only *reads* them (via [`RequiredSession`]) to obtain
//! the authenticated user's id.

use actixutils::Store;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

use crate::kv::KvStore;

/// Session payload stored server-side and referenced by the session cookie.
///
/// `user_id` is the authenticated subject (same role as JWT `sub`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthSession {
    pub user_id: Uuid,
}

/// Redis-backed [`Store`] for session data, keyed by session UUID.
///
/// Uses the shared [`KvStore`] so sessions are visible across replicas.
pub struct SessionStore {
    kv: Arc<dyn KvStore>,
    /// Prefix under the global Redis key prefix, e.g. `"session:"`.
    prefix: String,
    ttl: Duration,
}

impl SessionStore {
    pub fn new(kv: Arc<dyn KvStore>, prefix: impl Into<String>, ttl: Duration) -> Self {
        Self {
            kv,
            prefix: prefix.into(),
            ttl,
        }
    }

    fn key(&self, id: &Uuid) -> String {
        format!("{}{}", self.prefix, id)
    }
}

#[async_trait::async_trait]
impl Store<Uuid, AuthSession> for SessionStore {
    async fn get(&self, key: &Uuid) -> Result<Option<AuthSession>, Box<dyn Error>> {
        match self.kv.get(&self.key(key)).await? {
            Some(raw) => {
                let session: AuthSession = serde_json::from_str(&raw)?;
                Ok(Some(session))
            }
            None => Ok(None),
        }
    }

    async fn set(&self, key: &Uuid, value: AuthSession) -> Result<(), Box<dyn Error>> {
        let raw = serde_json::to_string(&value)?;
        self.kv.set(&self.key(key), &raw, self.ttl).await?;
        Ok(())
    }

    async fn delete(&self, key: &Uuid) -> Result<(), Box<dyn Error>> {
        self.kv.delete(&self.key(key)).await?;
        Ok(())
    }
}
