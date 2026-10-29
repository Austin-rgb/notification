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
use config::ModuleConfig;
use emailgrid::{
    Brevo, Console, EmailAddress, EmailingContext, Gmail, Resend, Sender as EmailSender,
};
use ferrumec::event::{EventStream, Identifier};
use ferrumec::{Infra, Module as FerrumecModule};
use mgk::{CreatePreference, GetAddress, IdResolver, Module as Mgk, Sender};
use push::{Config, NotificationRequest};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool, Pool, Postgres};
use std::sync::Arc;
use tagging::TagViewSet;
use uuid::Uuid;
use viewset::{DefaultRepo, Entity, ViewSet};

pub mod app;
pub mod config;
pub mod emailgrid;
pub mod session;

mod in_app;
mod mgk;
mod push;
mod tagging;

use ferrumec::cache::CacheFactory;

struct Push(Config);
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
        // The template builder needs a JSON object. Anything else (e.g. the bare
        // OTP "123456" sent by /preferences/set) is exposed to the template as
        // `{{ value }}`.
        let message = match serde_json::from_str::<serde_json::Value>(&message) {
            Ok(v) if v.is_object() => message,
            Ok(v) => serde_json::json!({ "value": v }).to_string(),
            Err(_) => serde_json::json!({ "value": message }).to_string(),
        };
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

/// The notification module: delivery channels (email, push), their
/// preference endpoints, notification tags and the websocket push endpoint.
///
/// Implements [`ferrumec::Module`], so it is built from any [`Infra`] (database
/// pool, cache factory, event stream) and mounted under the namespace it is
/// registered with, e.g. with `ferrumec::modules!`.
///
/// Every route lives behind [`RequiredSession`]: the session cookie is resolved
/// against the shared `session:` cache written by the auth service.
use viewset::web::SessionMiddleware;
pub struct NotificationModule {
    emailer: Mgk<EmailRepo>,
    push_mgk: Mgk<PushRepo>,
    push_: Config,
    in_app: in_app::InApp,
    tags: TagViewSet,
    session: Arc<SessionMiddleware>,
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
        let session = Arc::new(SessionMiddleware::new(kv.clone()));
        let emailer = build_emailer(cfg)?;
        let subjects = &cfg.subjects;
        let settings = cfg.settings.clone();

        let idres = Arc::new(MyIdResolver::new(pool.clone()));

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

        let in_app = in_app::InApp::new(
            pool.clone(),
            kv.clone(),
            es.clone(),
            idres.clone(),
            subjects.in_app.clone(),
        )
        .await;

        Ok(Self {
            emailer: email,
            push_mgk,
            push_,
            in_app,
            tags: tagging::create_tag_viewset(pool, kv),
            session,
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
    /// * `in-app` – `GET` the caller's stored in-app notifications
    /// * `email|push/preferences/{set,confirm,get}`
    fn configure(self: Arc<Self>, cfg: &mut ServiceConfig, namespace: &str) {
        cfg.service(
            web::scope(namespace)
                .wrap(self.session.clone())
                .configure(|cfg| self.tags.clone().configure(cfg, "tags"))
                .configure(|cfg| self.push_.config(cfg, "/ws"))
                .configure(|cfg| self.in_app.config(cfg, "/in-app"))
                .configure(|cfg| self.emailer.config(cfg, "/email"))
                .configure(|cfg| self.push_mgk.config(cfg, "/push")),
        );
    }
}

fn build_emailer(cfg: &ModuleConfig) -> Result<EmailingContext> {
    let e = &cfg.email;
    let api_key = || -> Result<String> {
        match &e.api_key {
            Some(k) => Ok(k.expose().to_string()),
            None => {
                Err(anyhow::anyhow!("email provider {:?} requires an API key", e.provider).into())
            }
        }
    };
    let sender: Arc<dyn EmailSender> = match e.provider {
        config::EmailProvider::Brevo => Arc::new(Brevo(api_key()?)),
        config::EmailProvider::Resend => Arc::new(Resend(api_key()?)),
        // Reads GMAIL_CLIENT_ID / GMAIL_CLIENT_SECRET / GMAIL_REFRESH_TOKEN;
        // `ModuleConfig::from_lookup` has already checked they are set.
        config::EmailProvider::Gmail => Arc::new(Gmail::new()?),
        config::EmailProvider::Console => Arc::new(Console::new()),
    };
    Ok(EmailingContext::new(
        sender,
        EmailAddress {
            email: e.from_address.clone(),
            name: e.from_name.clone(),
        },
    )?)
}
