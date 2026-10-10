//! A plain HTTP server for a set of [`ferrumec::Module`]s.
//!
//! ```text
//! AppConfig::from_env() ──► modules!(infra; ("notifications", Module)) ──► serve()
//! ```
//!
//! This is the standalone counterpart of `ferrumec::launch::launch`: it does
//! the same job (mount every module under its namespace, expose the infra
//! health check) without the global permission middleware or OpenTelemetry
//! bootstrap, so the notification service can run on its own. Each module still
//! guards its own routes (here: [`actixutils::middleware::RequiredSession`]).

use crate::config::AppConfig;
use actix_web::middleware::Logger;
use actix_web::{App, HttpServer};
use ferrumec::{Infra, Module};
use std::sync::Arc;

pub type Result<T> = core::result::Result<T, Box<dyn std::error::Error>>;

/// Bind and run until SIGINT/SIGTERM (actix drains in-flight requests).
///
/// Every `(namespace, module)` pair is mounted with [`Module::configure`] on
/// each worker, and `GET /healthz` reports the infrastructure status
/// (Postgres / Redis / NATS latencies, 503 when one is down).
pub async fn serve(
    cfg: &AppConfig,
    modules: Vec<(&'static str, Arc<dyn Module>)>,
    infra: impl Infra,
) -> Result<()> {
    let bind_addr = cfg.bind_addr.clone();
    let modules = Arc::new(modules);

    let mut server = HttpServer::new(move || {
        let mut app = App::new().wrap(Logger::default());
        for (namespace, module) in modules.iter() {
            app = app.configure(|c| module.clone().configure(c, namespace));
        }
        app.configure(|c| infra.configure(c, "/healthz"))
    });
    if let Some(n) = cfg.workers {
        server = server.workers(n);
    }
    let server = server.shutdown_timeout(30).bind(&bind_addr)?;

    tracing::info!(addr = %bind_addr, "notification service listening");
    server.run().await?;
    tracing::info!("notification service stopped");
    Ok(())
}
