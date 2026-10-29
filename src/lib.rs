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
use emailgrid::{Brevo, Console, EmailAddress, EmailingContext, Gmail, Resend, Sender as EmailSender};
use ferrumec::cache::CacheFactory;
use ferrumec::event::EventStream;
use ferrumec::{Infra, Module as FerrumecModule};
use mgk::Module as Mgk;
use push::Push;
use resolver::IdResolver;
use sqlx::{Pool, Postgres};
use std::sync::Arc;
use tagging::TagViewSet;
use viewset::web::SessionMiddleware;
use viewset::ViewSet;
pub use resolver::MyIdResolver;

pub mod app;
pub mod config;
pub mod emailgrid;
pub mod session;

mod in_app;
mod mgk;
mod push;
mod resolver;
mod tagging;

pub type Result<T> = core::result::Result<T, Box<dyn std::error::Error>>;

/// The notification module: delivery channels (email, push, in-app), the email
/// preference endpoints, notification tags and the websocket push endpoint.
///
/// Implements [`ferrumec::Module`], so it is built from any [`Infra`] (database
/// pool, cache factory, event stream) and mounted under the namespace it is
/// registered with, e.g. with `ferrumec::modules!`.
///
/// Every route lives behind [`RequiredSession`]: the session cookie is resolved
/// against the shared `session:` cache written by the auth service.
pub struct NotificationModule {
    email: Mgk,
    push: Push,
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
        let emailing = build_emailer(cfg)?;
        let subjects = &cfg.subjects;

        let idres: Arc<dyn IdResolver> = Arc::new(MyIdResolver::new(pool.clone()));

        let email = Mgk::new(
            pool.clone(),
            es.clone(),
            emailing,
            idres.clone(),
            subjects.email.clone(),
            kv.clone(),
            cfg.settings.clone(),
        )
        .await?;

        let push = Push::new(es.clone(), idres.clone(), subjects.push.clone()).await;

        let in_app = in_app::InApp::new(
            pool.clone(),
            kv.clone(),
            es.clone(),
            idres.clone(),
            subjects.in_app.clone(),
        )
        .await;

        Ok(Self {
            email,
            push,
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
    /// * `email/preferences/{set,confirm,get}`
    fn configure(self: Arc<Self>, cfg: &mut ServiceConfig, namespace: &str) {
        cfg.service(
            web::scope(namespace)
                .wrap(self.session.clone())
                .configure(|cfg| self.tags.clone().configure(cfg, "tags"))
                .configure(|cfg| self.push.config(cfg, "/ws"))
                .configure(|cfg| self.in_app.config(cfg, "/in-app"))
                .configure(|cfg| self.email.config(cfg, "/email")),
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
