//! Composition root: wire the infrastructure and start the web service.
//! All logic lives in the `notification` library.

use ferrumec::infra::dist::RemoteInfra;
use notification::app::NotificationService;
use notification::config::AppConfig;
use notification::telemetry;

#[actix_web::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    telemetry::init();

    let cfg = AppConfig::from_env()?; // env -> validated config
    let infra = RemoteInfra::from_env()
        .await
        .expect("could not connect infra"); // PostgreSQL + Redis + NATS (+ migrations)
    let service = NotificationService::build(&cfg, infra).await?; // channels, auth, email, push
    service.serve().await // HTTP server until SIGINT/SIGTERM
}
