//! Application assembly: turns validated config + connected infrastructure
//! into a running HTTP service.
//!
//! ```text
//! AppConfig::from_env() ──► Infra::connect(&cfg) ──► NotificationService::build ──► .serve()
//! ```

use crate::Module;
use crate::config::{AppConfig, EmailProvider};
use crate::emailgrid::{Brevo, EmailAddress, EmailingContext, Resend, Sender as EmailSender};
use crate::infra::Infra;
use crate::kv::KvStore;
use crate::session::{AuthSession, SessionStore};
use actix_web::middleware::Logger;
use actix_web::{HttpResponse, HttpServer, web};
use actixutils::middleware::RequiredSession;
use anyhow::Result;
use sqlx::PgPool;
use std::sync::Arc;

/// What the readiness probe checks.
struct Health {
    pool: PgPool,
    kv: Arc<dyn KvStore>,
}

pub struct NotificationService {
    cfg: AppConfig,
    module: Module,
    session_store: Arc<SessionStore>,
    health: web::Data<Health>,
}

impl NotificationService {
    /// Wire the module (channels, preferences, push, tags) onto the connected infrastructure.
    pub async fn build(cfg: &AppConfig, infra: Infra) -> Result<Self> {
        let Infra { pool, kv, events } = infra;

        let session_store = Arc::new(SessionStore::new(kv.clone(), "session:", cfg.session_ttl));

        let module = Module::new(
            pool.clone(),
            build_emailer(cfg)?,
            events,
            kv.clone(),
            &cfg.subjects,
            cfg.settings.clone(),
        )
        .await?;

        Ok(Self {
            cfg: cfg.clone(),
            module,
            session_store,
            health: web::Data::new(Health { pool, kv }),
        })
    }

    /// Register every route and its shared state. Reusable if you want to
    /// mount the service inside a bigger actix `App`.
    pub fn configure(&self, cfg: &mut web::ServiceConfig) {
        let cookie_name = self.cfg.session_cookie.clone();
        let session_store = self.session_store.clone();

        cfg.app_data(self.health.clone())
            .route("/healthz", web::get().to(healthz))
            .route("/readyz", web::get().to(readyz))
            // Authenticated API: session cookie required.
            .service(
                web::scope(&self.cfg.api_prefix)
                    .wrap(
                        RequiredSession::<AuthSession>::new(session_store).cookie_name(cookie_name),
                    )
                    .configure(|c| self.module.config(c, "")),
            );
    }

    /// Bind and run until SIGINT/SIGTERM (actix drains in-flight requests).
    pub async fn serve(self) -> Result<()> {
        let bind_addr = self.cfg.bind_addr.clone();
        let workers = self.cfg.workers;
        let prefix = self.cfg.api_prefix.clone();
        let service = Arc::new(self);

        let mut server = HttpServer::new(move || {
            let service = service.clone();
            actix_web::App::new()
                .wrap(Logger::default())
                .configure(move |cfg| service.configure(cfg))
        });
        if let Some(n) = workers {
            server = server.workers(n);
        }
        let server = server.shutdown_timeout(30).bind(&bind_addr)?;

        tracing::info!(addr = %bind_addr, api_prefix = %prefix, "notification service listening");
        server.run().await?;
        tracing::info!("notification service stopped");
        Ok(())
    }
}

fn build_emailer(cfg: &AppConfig) -> Result<EmailingContext> {
    let e = &cfg.email;
    let sender: Arc<dyn EmailSender> = match e.provider {
        EmailProvider::Brevo => Arc::new(Brevo::with_config(
            e.api_key.expose().to_string(),
            e.api_url.clone(),
        )),
        EmailProvider::Resend => Arc::new(Resend::with_config(
            e.api_key.expose().to_string(),
            e.api_url.clone(),
        )),
    };
    EmailingContext::new(
        sender,
        EmailAddress {
            email: e.from_address.clone(),
            name: e.from_name.clone(),
        },
    )
}

/// Liveness: the process is up.
async fn healthz() -> HttpResponse {
    HttpResponse::Ok().json(serde_json::json!({ "status": "ok" }))
}

/// Readiness: Postgres and Redis are reachable.
/// (NATS is not probed: the client reconnects on its own and exposes no cheap check.)
async fn readyz(state: web::Data<Health>) -> HttpResponse {
    let postgres = sqlx::query("SELECT 1").execute(&state.pool).await.is_ok();
    let redis = state.kv.ping().await.is_ok();
    let body = serde_json::json!({
        "status": if postgres && redis { "ok" } else { "unavailable" },
        "postgres": postgres,
        "redis": redis,
    });
    if postgres && redis {
        HttpResponse::Ok().json(body)
    } else {
        HttpResponse::ServiceUnavailable().json(body)
    }
}
