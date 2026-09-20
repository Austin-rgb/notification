//! Environment-driven configuration.
//!
//! All environment access lives here. The rest of the crate never calls
//! `std::env`, so a misconfigured deployment fails at startup with a message
//! naming the offending variable instead of panicking later.
//!
//! See `README.md` for the full variable table.

use anyhow::{Result, bail};
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

/// A value that must never appear in logs (URLs with passwords, signing keys, API keys).
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// Event-stream subjects each delivery channel subscribes to.
#[derive(Debug, Clone)]
pub struct Subjects {
    pub email: Vec<String>,
    pub push: Vec<String>,
    pub console: Vec<String>,
}

/// Tunables for the preference / OTP flow.
#[derive(Debug, Clone)]
pub struct Settings {
    /// How long a resolved (user, subject) -> address lookup stays in Redis.
    pub cache_ttl: Duration,
    /// How long an OTP confirmation stays valid.
    pub otp_ttl: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cache_ttl: Duration::from_secs(3600),
            otp_ttl: Duration::from_secs(300),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailProvider {
    Brevo,
    Resend,
}

#[derive(Debug, Clone)]
pub struct EmailConfig {
    pub provider: EmailProvider,
    pub api_key: Secret,
    /// Override the provider endpoint (used by the e2e suite to point at a stub).
    pub api_url: Option<String>,
    pub from_address: String,
    pub from_name: String,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub bind_addr: String,
    pub workers: Option<usize>,
    /// URL prefix the whole API is mounted under, e.g. `/notifications`.
    pub api_prefix: String,

    pub database_url: Secret,
    pub db_max_connections: u32,
    pub run_migrations: bool,

    pub redis_url: Secret,
    pub redis_key_prefix: String,

    pub nats_url: Secret,

    /// Total time to keep retrying Postgres / Redis / NATS at boot.
    pub startup_timeout: Duration,

    /// How long a server-side session entry remains valid in Redis.
    pub session_ttl: Duration,
    /// Cookie name used for the session id (must match the auth service).
    pub session_cookie: String,

    pub subjects: Subjects,
    pub settings: Settings,
    pub email: EmailConfig,
}

fn parse_or<T>(key: &str, raw: Option<String>, default: T) -> Result<T>
where
    T: FromStr,
    T::Err: fmt::Display,
{
    match raw {
        None => Ok(default),
        Some(v) => v
            .parse::<T>()
            .map_err(|e| anyhow::anyhow!("{key}: invalid value {v:?}: {e}")),
    }
}

fn parse_subjects(key: &str, raw: &str) -> Result<Vec<String>> {
    let items: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if items.is_empty() {
        bail!("{key} must contain at least one subject (comma-separated)");
    }
    if let Some(bad) = items.iter().find(|s| s.chars().any(char::is_whitespace)) {
        bail!("{key}: subject {bad:?} must not contain whitespace");
    }
    Ok(items)
}

impl AppConfig {
    /// Load from the process environment.
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Load from any key/value source (handy for tests).
    pub fn from_lookup<F>(get: F) -> Result<Self>
    where
        F: Fn(&str) -> Option<String>,
    {
        let optional = |key: &str| -> Option<String> {
            get(key)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let required = |key: &str| -> Result<String> {
            match optional(key) {
                Some(v) => Ok(v),
                None => bail!("{key} must be set"),
            }
        };

        let mut api_prefix = optional("API_PREFIX").unwrap_or_else(|| "/notifications".to_string());
        if !api_prefix.starts_with('/') {
            api_prefix.insert(0, '/');
        }
        while api_prefix.len() > 1 && api_prefix.ends_with('/') {
            api_prefix.pop();
        }
        if api_prefix == "/" {
            api_prefix.clear();
        }

        let workers = match optional("WORKERS") {
            None => None,
            Some(v) => {
                let n: usize = parse_or("WORKERS", Some(v), 1)?;
                if n == 0 {
                    bail!("WORKERS must be greater than zero");
                }
                Some(n)
            }
        };

        let provider = match required("EMAIL_PROVIDER")?.to_ascii_lowercase().as_str() {
            "brevo" => EmailProvider::Brevo,
            "resend" => EmailProvider::Resend,
            other => bail!("EMAIL_PROVIDER must be 'brevo' or 'resend', got {other:?}"),
        };
        let (key_var, url_var) = match provider {
            EmailProvider::Brevo => ("BREVO_API_KEY", "BREVO_API_URL"),
            EmailProvider::Resend => ("RESEND_API_KEY", "RESEND_API_URL"),
        };

        let defaults = Settings::default();
        let cache_ttl = parse_or(
            "CACHE_TTL_SECS",
            optional("CACHE_TTL_SECS"),
            defaults.cache_ttl.as_secs(),
        )?;
        let otp_ttl = parse_or(
            "OTP_TTL_SECS",
            optional("OTP_TTL_SECS"),
            defaults.otp_ttl.as_secs(),
        )?;

        Ok(Self {
            bind_addr: optional("BIND_ADDR").unwrap_or_else(|| "0.0.0.0:8080".to_string()),
            workers,
            api_prefix,
            database_url: Secret::new(required("DATABASE_URL")?),
            db_max_connections: parse_or(
                "DB_MAX_CONNECTIONS",
                optional("DB_MAX_CONNECTIONS"),
                10u32,
            )?,
            run_migrations: parse_or("RUN_MIGRATIONS", optional("RUN_MIGRATIONS"), true)?,
            redis_url: Secret::new(required("REDIS_URL")?),
            redis_key_prefix: optional("REDIS_KEY_PREFIX")
                .unwrap_or_else(|| "notification:".to_string()),
            nats_url: Secret::new(required("NATS_URL")?),
            startup_timeout: Duration::from_secs(parse_or(
                "STARTUP_TIMEOUT_SECS",
                optional("STARTUP_TIMEOUT_SECS"),
                60u64,
            )?),
            session_ttl: Duration::from_secs(parse_or(
                "SESSION_TTL_SECS",
                optional("SESSION_TTL_SECS"),
                86400u64, // 24h
            )?),
            session_cookie: optional("SESSION_COOKIE").unwrap_or_else(|| "session".to_string()),
            subjects: Subjects {
                email: parse_subjects("EMAIL_SUBJECTS", &required("EMAIL_SUBJECTS")?)?,
                push: parse_subjects("PUSH_SUBJECTS", &required("PUSH_SUBJECTS")?)?,
                console: parse_subjects("CONSOLE_SUBJECTS", &required("CONSOLE_SUBJECTS")?)?,
            },
            settings: Settings {
                cache_ttl: Duration::from_secs(cache_ttl),
                otp_ttl: Duration::from_secs(otp_ttl),
            },
            email: EmailConfig {
                provider,
                api_key: Secret::new(required(key_var)?),
                api_url: optional(url_var),
                from_address: required("EMAIL_FROM_ADDRESS")?,
                from_name: optional("EMAIL_FROM_NAME")
                    .unwrap_or_else(|| "Notifications".to_string()),
            },
        })
    }
}
