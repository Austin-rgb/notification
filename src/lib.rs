//! # notification
//!
//! Notification service library. Everything the service does lives here so it
//! can be reused (embedded in another binary, driven from tests); `main.rs` is
//! only the composition root.
//!
//! * [`config`]    – environment-driven configuration
//! * [`infra`]     – PostgreSQL, Redis and NATS connections
//! * [`kv`]        – shared key/value store (Redis) for cache + pending OTPs
//! * [`app`]       – builds the HTTP application and serves it
//! * [`telemetry`] – logging
//! * [`Module`]    – the notification module: channels, preferences, tags, push

use actix_web::{web, web::ServiceConfig};
use anyhow::Result;
use emailgrid::EmailingContext;
use mgk::{CreatePreference, GetAddress, IdResolver, Module as Mgk, Sender};
use push::{Config, NotificationRequest};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool, Pool, Postgres};
use std::sync::Arc;
use typed_eventbus::{EventStream, Identifier};
use uuid::Uuid;
use viewset::{DefaultRepo, Entity, ViewSet};

pub mod app;
pub mod config;
pub mod emailgrid;
pub mod infra;
pub mod kv;
pub mod session;
pub mod telemetry;

mod mgk;
mod push;
mod tagging;

use config::{Settings, Subjects};
use kv::KvStore;

struct Push(Config);
struct Console;
struct Email(EmailingContext);

// NOTE on the `user_id` field below: it is deliberately not called `user`.
// `user` is a reserved word in PostgreSQL and viewset's generated SQL does not
// quote identifiers. `subject` and `user_id` are `filterable` because
// `Preferences::get` looks rows up with them — viewset silently ignores
// filters on columns that are not declared filterable.

#[derive(Entity, Serialize, Deserialize, Clone, FromRow)]
#[entity(table = "email_preferences", create = "CreatePreference")]
struct EmailPreference {
    #[entity(skip_create)]
    id: Uuid,
    #[entity(filterable)]
    pub subject: String,
    pub address: String,
    #[entity(filterable)]
    pub user_id: String,
}

impl GetAddress for EmailPreference {
    fn get_address(&self) -> String {
        self.address.clone()
    }
}

type EmailRepo = DefaultRepo<EmailPreference>;

#[derive(Entity, Serialize, Deserialize, Clone, FromRow)]
#[entity(table = "push_preferences", create = "CreatePreference")]
struct PushPreference {
    #[entity(skip_create)]
    id: Uuid,
    #[entity(filterable)]
    pub subject: String,
    pub address: String,
    #[entity(filterable)]
    pub user_id: String,
}

impl GetAddress for PushPreference {
    fn get_address(&self) -> String {
        self.address.clone()
    }
}

type PushRepo = DefaultRepo<PushPreference>;

#[derive(Entity, Serialize, Deserialize, Clone, FromRow)]
#[entity(table = "console_preferences", create = "CreatePreference")]
struct ConsolePreference {
    #[entity(skip_create)]
    id: Uuid,
    #[entity(filterable)]
    pub subject: String,
    pub address: String,
    #[entity(filterable)]
    pub user_id: String,
}

impl GetAddress for ConsolePreference {
    fn get_address(&self) -> String {
        self.address.clone()
    }
}

type ConsoleRepo = DefaultRepo<ConsolePreference>;

pub struct MyIdResolver {
    pool: PgPool,
}

impl MyIdResolver {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl IdResolver for MyIdResolver {
    async fn resolve(&self, id: Identifier) -> Result<Uuid> {
        match id {
            Identifier::Uuid(id) => Ok(id),

            Identifier::Tag(tag) => {
                let user_id: Uuid =
                    sqlx::query_scalar("SELECT user_id FROM notification_tags WHERE tag = $1")
                        .bind(tag)
                        .fetch_one(&self.pool)
                        .await?;

                Ok(user_id)
            }
        }
    }
}

#[async_trait::async_trait]
impl Sender for Push {
    async fn send(&self, address: String, _subject: String, message: String) -> Result<()> {
        let notification = NotificationRequest {
            message,
            targets: vec![address],
        };
        self.0.push("push".to_string(), notification);
        Ok(())
    }
}

#[async_trait::async_trait]
impl Sender for Email {
    async fn send(&self, address: String, subject: String, message: String) -> Result<()> {
        // Template / build errors surface here...
        let response = self.0.send(address, subject, message).await?;
        // ...and so do transport errors and non-2xx answers from the provider, so
        // the caller can log them instead of silently dropping the email.
        match response {
            Ok(resp) if resp.status().is_success() => Ok(()),
            Ok(resp) => Err(anyhow::anyhow!(
                "email provider responded with status {}",
                resp.status()
            )),
            Err(e) => Err(anyhow::anyhow!("email provider request failed: {e}")),
        }
    }
}

#[async_trait::async_trait]
impl Sender for Console {
    async fn send(&self, address: String, subject: String, message: String) -> Result<()> {
        println!("message sent: address = {address}, subject = {subject}, message = {message}");
        Ok(())
    }
}

pub struct Module {
    emailer: Mgk<EmailRepo>,
    push_mgk: Mgk<PushRepo>,
    push_: Config,
    console: Mgk<ConsoleRepo>,
    pool: Pool<Postgres>,
}

impl Module {
    /// Build the module and subscribe every channel to its subjects on `es`.
    ///
    /// Must be called from inside a running actix system (the push channel
    /// starts an actor).
    pub async fn new(
        pool: Pool<Postgres>,
        emailer: EmailingContext,
        es: Arc<dyn EventStream>,
        kv: Arc<dyn KvStore>,
        subjects: &Subjects,
        settings: Settings,
    ) -> anyhow::Result<Self> {
        let idres = Arc::new(MyIdResolver::new(pool.clone()));
        let repo: ConsoleRepo = pool.clone().into();
        let console = Mgk::new(
            repo.into(),
            es.clone(),
            Arc::new(Console {}),
            idres.clone(),
            subjects.console.clone(),
            kv.clone(),
            settings.clone(),
        )
        .await?;
        let push_ = Config::new().await;
        let repo: PushRepo = pool.clone().into();
        let push_mgk = Mgk::new(
            repo.into(),
            es.clone(),
            Arc::new(Push(push_.clone())),
            idres.clone(),
            subjects.push.clone(),
            kv.clone(),
            settings.clone(),
        )
        .await?;
        let repo: EmailRepo = pool.clone().into();
        let email = Mgk::new(
            repo.into(),
            es.clone(),
            Arc::new(Email(emailer)) as Arc<dyn Sender>,
            idres.clone(),
            subjects.email.clone(),
            kv,
            settings,
        )
        .await?;

        Ok(Self {
            emailer: email,
            push_mgk,
            push_,
            console,
            pool,
        })
    }

    /// Register channel routes on `cfg`.
    ///
    /// When `namespace` is empty the routes are mounted at the root of the
    /// current scope (used when the outer app already scoped under
    /// `api_prefix` and applied session middleware). Otherwise a nested
    /// scope is created under `namespace`.
    pub fn config(&self, cfg: &mut ServiceConfig, namespace: &str) {
        cfg.app_data(web::Data::new(self.pool.clone()));
        if namespace.is_empty() {
            tagging::create_tag_viewset(self.pool.clone()).configure(cfg, "tags");
            self.push_.config(cfg, "/ws");
            self.emailer.config(cfg, "/email");
            self.push_mgk.config(cfg, "/push");
            self.console.config(cfg, "/console");
        } else {
            cfg.service(
                web::scope(namespace)
                    .configure(|cfg| {
                        tagging::create_tag_viewset(self.pool.clone()).configure(cfg, "tags")
                    })
                    .configure(|cfg| self.push_.config(cfg, "/ws"))
                    .configure(|cfg| self.emailer.config(cfg, "/email"))
                    .configure(|cfg| self.push_mgk.config(cfg, "/push"))
                    .configure(|cfg| self.console.config(cfg, "/console")),
            );
        }
    }
}
