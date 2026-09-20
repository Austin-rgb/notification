//! Composition root: wire the infrastructure and start the web service.
//! All logic lives in the `notification` library.

use notification::app::NotificationService;
use notification::config::AppConfig;
use notification::infra::Infra;
use notification::telemetry;

#[actix_web::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init();

    let cfg = AppConfig::from_env()?; // env -> validated config
    let infra = Infra::connect(&cfg).await?; // PostgreSQL + Redis + NATS (+ migrations)
    let service = NotificationService::build(&cfg, infra).await?; // channels, auth, email, push
    service.serve().await // HTTP server until SIGINT/SIGTERM
}
