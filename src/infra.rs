//! Connections to the production infrastructure: PostgreSQL (database),
//! Redis (cache) and NATS (event stream). Each is retried with backoff at boot
//! so the service tolerates dependencies that come up a few seconds late.

use crate::config::AppConfig;
use crate::kv::{KvStore, RedisStore};
use anyhow::{Context, Result};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use typed_eventbus::{EventStream, NatsEventStream};

/// Everything the service needs from the outside world.
pub struct Infra {
    pub pool: PgPool,
    pub kv: Arc<dyn KvStore>,
    pub events: Arc<dyn EventStream>,
}

impl Infra {
    /// Connect to Postgres, run migrations (unless disabled), then Redis and NATS.
    pub async fn connect(cfg: &AppConfig) -> Result<Self> {
        let timeout = cfg.startup_timeout;

        let pool = retry("postgres", timeout, || connect_postgres(cfg)).await?;
        if cfg.run_migrations {
            sqlx::migrate!("./migrations")
                .run(&pool)
                .await
                .context("database migrations failed")?;
            tracing::info!("database migrations applied");
        } else {
            tracing::info!("RUN_MIGRATIONS=false, skipping migrations");
        }

        let redis = retry("redis", timeout, || {
            RedisStore::connect(cfg.redis_url.expose(), cfg.redis_key_prefix.clone())
        })
        .await?;
        let kv: Arc<dyn KvStore> = Arc::new(redis);

        let events = retry("nats", timeout, || connect_nats(cfg.nats_url.expose())).await?;

        Ok(Self { pool, kv, events })
    }
}

async fn connect_postgres(cfg: &AppConfig) -> Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(cfg.db_max_connections)
        .acquire_timeout(Duration::from_secs(5))
        .connect(cfg.database_url.expose())
        .await
        .context("cannot connect to PostgreSQL")?;
    Ok(pool)
}

async fn connect_nats(url: &str) -> Result<Arc<dyn EventStream>> {
    // Deliberately not including `url` in the error: it may carry credentials.
    let stream = NatsEventStream::new(url)
        .await
        .map_err(|e| anyhow::anyhow!("cannot connect to NATS: {e}"))?;
    let events: Arc<dyn EventStream> = Arc::new(stream);
    Ok(events)
}

/// Retry `op` with exponential backoff (250ms .. 5s) until it succeeds or
/// `timeout` elapses.
async fn retry<T, F, Fut>(name: &str, timeout: Duration, mut op: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let started = Instant::now();
    let mut delay = Duration::from_millis(250);
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match op().await {
            Ok(value) => {
                tracing::info!(dependency = name, attempt, "connected");
                return Ok(value);
            }
            Err(e) if started.elapsed() + delay < timeout => {
                tracing::warn!(dependency = name, attempt, error = %e, "not ready, retrying");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(5));
            }
            Err(e) => {
                return Err(e.context(format!("{name}: giving up after {attempt} attempts")));
            }
        }
    }
}
