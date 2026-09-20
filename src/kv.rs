//! Shared key/value store used for the preference cache and pending OTP
//! confirmations.
//!
//! Production uses Redis so that state is shared by every replica (an OTP
//! requested on replica A can be confirmed on replica B, and a confirmed
//! address is visible everywhere immediately). The trait exists so tests can
//! inject a fake without a Redis server.

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::time::Duration;

#[async_trait]
pub trait KvStore: Send + Sync {
    async fn get(&self, key: &str) -> Result<Option<String>>;
    /// Store `value` under `key`, expiring after `ttl` (minimum one second).
    async fn set(&self, key: &str, value: &str, ttl: Duration) -> Result<()>;
    async fn delete(&self, key: &str) -> Result<()>;
    /// Cheap liveness probe used by `/readyz`.
    async fn ping(&self) -> Result<()>;
}

/// Redis-backed [`KvStore`]. Cloning is cheap; the connection manager
/// reconnects automatically after network blips.
#[derive(Clone)]
pub struct RedisStore {
    conn: redis::aio::ConnectionManager,
    prefix: String,
}

impl RedisStore {
    pub async fn connect(url: &str, prefix: impl Into<String>) -> Result<Self> {
        let client = redis::Client::open(url).context("REDIS_URL is not a valid Redis URL")?;
        let conn = redis::aio::ConnectionManager::new(client)
            .await
            .context("cannot connect to Redis")?;
        let store = Self {
            conn,
            prefix: prefix.into(),
        };
        store.ping().await?;
        Ok(store)
    }

    fn key(&self, key: &str) -> String {
        format!("{}{}", self.prefix, key)
    }
}

#[async_trait]
impl KvStore for RedisStore {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        let mut conn = self.conn.clone();
        let value: Option<String> = redis::cmd("GET")
            .arg(self.key(key))
            .query_async(&mut conn)
            .await
            .context("redis GET failed")?;
        Ok(value)
    }

    async fn set(&self, key: &str, value: &str, ttl: Duration) -> Result<()> {
        let mut conn = self.conn.clone();
        let _: () = redis::cmd("SET")
            .arg(self.key(key))
            .arg(value)
            .arg("EX")
            .arg(ttl.as_secs().max(1))
            .query_async(&mut conn)
            .await
            .context("redis SET failed")?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let mut conn = self.conn.clone();
        let _: i64 = redis::cmd("DEL")
            .arg(self.key(key))
            .query_async(&mut conn)
            .await
            .context("redis DEL failed")?;
        Ok(())
    }

    async fn ping(&self) -> Result<()> {
        let mut conn = self.conn.clone();
        let _: String = redis::cmd("PING")
            .query_async(&mut conn)
            .await
            .context("redis PING failed")?;
        Ok(())
    }
}
