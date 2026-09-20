//! Logging setup. `RUST_LOG` controls verbosity (default `info`);
//! `LOG_FORMAT=json` switches to structured logs for log aggregation.

pub fn init() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    let json = std::env::var("LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false);
    if json {
        builder.json().init();
    } else {
        builder.init();
    }
}
