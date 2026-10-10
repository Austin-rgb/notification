//! # notification
//!
//! Notification service library. Everything the service does lives here so it
//! can be reused (embedded in another binary, driven from tests); `main.rs` is
//! only the composition root.
//!
//! * [`config`]    – environment-driven configuration
//! * [`app`]       – a plain HTTP server for a set of [`ferrumec::Module`]s
//! * [`telemetry`] – logging
//! * [`Module`]    – the notification module (a [`ferrumec::Module`]): channels,
//!   preferences, tags, push. Built from any [`ferrumec::Infra`], so it runs on
//!   `RemoteInfra` (Postgres + Redis + NATS) in production and `LocalInfra` in
//!   development and tests.

use actix_web::{web, web::ServiceConfig};
use actixutils::middleware::RequiredSession;
use config::ModuleConfig;
use emailgrid::{Brevo, EmailAddress, EmailingContext, Resend, Sender as EmailSender};
use ferrumec::event::{EventStream, Identifier};
use ferrumec::{Infra, Module as FerrumecModule, Store as KvStore};
use mgk::{CreatePreference, GetAddress, IdResolver, Module as Mgk, Sender};
use push::{Config, NotificationRequest};
use serde::{Deserialize, Serialize};
use session::AuthSession;
use sqlx::{FromRow, PgPool, Pool, Postgres};
use std::sync::Arc;
use tagging::TagViewSet;
use uuid::Uuid;
use viewset::{DefaultRepo, Entity, ViewSet};

pub mod app;
pub mod config;
pub mod emailgrid;
pub mod session;
pub mod telemetry;

mod mgk;
mod push;
mod tagging;

use ferrumec::cache::CacheFactory;

struct Push(Config);
struct Console;
struct Email(EmailingContext);

pub type Result<T> = core::result::Result<T, Box<dyn std::error::Error>>;

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
            )
            .into()),
            Err(e) => Err(anyhow::anyhow!("email provider request failed: {e}").into()),
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

/// The notification module: delivery channels (email, push, console), their
/// preference endpoints, notification tags and the websocket push endpoint.
///
/// Implements [`ferrumec::Module`], so it is built from any [`Infra`] (database
/// pool, cache factory, event stream) and mounted under the namespace it is
/// registered with, e.g. with `ferrumec::modules!`.
///
/// Every route lives behind [`RequiredSession`]: the session cookie is resolved
/// against the shared `session:` cache written by the auth service.
pub struct NotificationModule {
    emailer: Mgk<EmailRepo>,
    push_mgk: Mgk<PushRepo>,
    push_: Config,
    console: Mgk<ConsoleRepo>,
    tags: TagViewSet,
    session_store: Arc<dyn KvStore<Uuid, AuthSession>>,
    session_cookie: String,
}

/// The name this module is referred to by, matching the other services
/// (`authz::Module`).
pub use NotificationModule as Module;

impl NotificationModule {
    /// Build the module from explicit parts and subscribe every channel to its
    /// subjects on `es`.
    ///
    /// This is what [`ferrumec::Module::new`] calls after reading
    /// [`ModuleConfig`] from the environment; use it directly when you already
    /// have a config (tests, embedding).
    ///
    /// Must be called from inside a running actix system (the push channel
    /// starts an actor).
    pub async fn build(
        pool: Pool<Postgres>,
        es: Arc<dyn EventStream>,
        kv: impl CacheFactory,
        cfg: &ModuleConfig,
    ) -> Result<Self> {
        let session_store = kv.new_cache("session:", cfg.session_ttl);
        let emailer = build_emailer(cfg)?;
        let subjects = &cfg.subjects;
        let settings = cfg.settings.clone();

        let idres = Arc::new(MyIdResolver::new(pool.clone()));

        let repo = ConsoleRepo::new(pool.clone(), kv.clone());
        let console = Mgk::new(
            Arc::new(repo),
            es.clone(),
            Arc::new(Console {}),
            idres.clone(),
            subjects.console.clone(),
            kv.clone(),
            settings.clone(),
        )
        .await?;

        let push_ = Config::new().await;
        let repo = PushRepo::new(pool.clone(), kv.clone());
        let push_mgk = Mgk::new(
            Arc::new(repo),
            es.clone(),
            Arc::new(Push(push_.clone())),
            idres.clone(),
            subjects.push.clone(),
            kv.clone(),
            settings.clone(),
        )
        .await?;

        let repo = EmailRepo::new(pool.clone(), kv.clone());
        let email = Mgk::new(
            Arc::new(repo),
            es.clone(),
            Arc::new(Email(emailer)) as Arc<dyn Sender>,
            idres.clone(),
            subjects.email.clone(),
            kv.clone(),
            settings,
        )
        .await?;

        Ok(Self {
            emailer: email,
            push_mgk,
            push_,
            console,
            tags: tagging::create_tag_viewset(pool, kv),
            session_store,
            session_cookie: cfg.session_cookie.clone(),
        })
    }
}

impl FerrumecModule for NotificationModule {
    /// Builds the module from `infra`, reading channel subjects, OTP/cache
    /// tunables, the email provider and the session settings from the
    /// environment (see [`ModuleConfig::from_env`]).
    async fn new(infra: impl Infra) -> Result<Self> {
        let cfg = ModuleConfig::from_env()?;
        Self::build(
            infra.database(),
            infra.event_stream(),
            infra.cache_factory(),
            &cfg,
        )
        .await
    }

    /// Mounts everything under `namespace`, behind the session middleware:
    ///
    /// * `tags` – notification tag CRUD
    /// * `ws/` – websocket push
    /// * `email|push|console/preferences/{set,confirm,get}`
    fn configure(self: Arc<Self>, cfg: &mut ServiceConfig, namespace: &str) {
        let session = RequiredSession::<AuthSession>::new(self.session_store.clone())
            .cookie_name(self.session_cookie.clone());
        cfg.service(
            web::scope(namespace)
                .wrap(session)
                .configure(|cfg| self.tags.clone().configure(cfg, "tags"))
                .configure(|cfg| self.push_.config(cfg, "/ws"))
                .configure(|cfg| self.emailer.config(cfg, "/email"))
                .configure(|cfg| self.push_mgk.config(cfg, "/push"))
                .configure(|cfg| self.console.config(cfg, "/console")),
        );
    }
}

fn build_emailer(cfg: &ModuleConfig) -> Result<EmailingContext> {
    let e = &cfg.email;
    let sender: Arc<dyn EmailSender> = match e.provider {
        config::EmailProvider::Brevo => Arc::new(Brevo::with_config(
            e.api_key.expose().to_string(),
            e.api_url.clone(),
        )),
        config::EmailProvider::Resend => Arc::new(Resend::with_config(
            e.api_key.expose().to_string(),
            e.api_url.clone(),
        )),
    };
    Ok(EmailingContext::new(
        sender,
        EmailAddress {
            email: e.from_address.clone(),
            name: e.from_name.clone(),
        },
    )?)
}
