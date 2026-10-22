//! Environment-driven configuration.
//!
//! All environment access lives here. The rest of the crate never calls
//! `std::env`, so a misconfigured deployment fails at startup with a message
//! naming the offending variable instead of panicking later.
//!
//! Two configs, mirroring the two layers:
//!
//! * [`ModuleConfig`] – everything the [`crate::Module`] needs (channel subjects,
//!   OTP/cache tunables, email provider, session settings). Read by
//!   `ferrumec::Module::new`.
//! * [`AppConfig`] – only the HTTP server around the modules (bind address,
//!   workers). Postgres / Redis / NATS connections are owned by the
//!   `ferrumec::Infra` implementation (`RemoteInfra::from_env` reads
//!   `DATABASE_URL`, `REDIS_URL` and `NATS_URL`).
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
    /// Gmail API (OAuth2 refresh token). Needs `GMAIL_CLIENT_ID`,
    /// `GMAIL_CLIENT_SECRET` and `GMAIL_REFRESH_TOKEN`.
    Gmail,
    /// Prints emails to stdout instead of sending them (development).
    Console,
}

#[derive(Debug, Clone)]
pub struct EmailConfig {
    pub provider: EmailProvider,
    /// API key for providers that use one (Brevo, Resend); `None` for Gmail
    /// (OAuth credentials) and Console.
    pub api_key: Option<Secret>,
    pub from_address: String,
    pub from_name: String,
}

/// Server-level settings: where to listen and under which namespace the
/// notification module is mounted.
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub bind_addr: String,
    pub workers: Option<usize>,
    /// Namespace (URL prefix, no slashes) the notification module is
    /// registered under, e.g. `notifications`.
    pub namespace: String,
}

/// Everything [`crate::Module`] needs to build itself.
#[derive(Debug, Clone)]
pub struct ModuleConfig {
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

fn lookup_fns<'a, F>(
    get: &'a F,
) -> (
    impl Fn(&str) -> Option<String> + 'a,
    impl Fn(&str) -> Result<String> + 'a,
)
where
    F: Fn(&str) -> Option<String>,
{
    let optional = move |key: &str| -> Option<String> {
        get(key)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let required = move |key: &str| -> Result<String> {
        match optional(key) {
            Some(v) => Ok(v),
            None => bail!("{key} must be set"),
        }
    };
    (optional, required)
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
        let (optional, _) = lookup_fns(&get);

        // `API_PREFIX` is still accepted (e.g. `/notifications/`); it is now the
        // module namespace, stored without surrounding slashes.
        let namespace = optional("API_PREFIX")
            .unwrap_or_else(|| "notifications".to_string())
            .trim_matches('/')
            .to_string();

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

        Ok(Self {
            bind_addr: optional("BIND_ADDR").unwrap_or_else(|| "0.0.0.0:8080".to_string()),
            workers,
            namespace,
        })
    }
}

impl ModuleConfig {
    /// Load from the process environment.
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Load from any key/value source (handy for tests).
    pub fn from_lookup<F>(get: F) -> Result<Self>
    where
        F: Fn(&str) -> Option<String>,
    {
        let (optional, required) = lookup_fns(&get);

        let provider = match required("EMAIL_PROVIDER")?.to_ascii_lowercase().as_str() {
            "brevo" => EmailProvider::Brevo,
            "resend" => EmailProvider::Resend,
            "gmail" => EmailProvider::Gmail,
            "console" => EmailProvider::Console,
            other => bail!(
                "EMAIL_PROVIDER must be 'brevo', 'resend', 'gmail' or 'console', got {other:?}"
            ),
        };
        let api_key = match provider {
            EmailProvider::Brevo => Some(Secret::new(required("BREVO_API_KEY")?)),
            EmailProvider::Resend => Some(Secret::new(required("RESEND_API_KEY")?)),
            EmailProvider::Gmail => {
                // `emailgrid::Gmail::new` reads these itself; fail at startup
                // with the variable name rather than later with a bare VarError.
                for key in [
                    "GMAIL_CLIENT_ID",
                    "GMAIL_CLIENT_SECRET",
                    "GMAIL_REFRESH_TOKEN",
                ] {
                    required(key)?;
                }
                None
            }
            EmailProvider::Console => None,
        };
        let from_address = match provider {
            EmailProvider::Console => {
                optional("EMAIL_FROM_ADDRESS").unwrap_or_else(|| "noreply@localhost".to_string())
            }
            _ => required("EMAIL_FROM_ADDRESS")?,
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
            session_ttl: Duration::from_secs(parse_or(
                "SESSION_TTL_SECS",
                optional("SESSION_TTL_SECS"),
                86400u64, // 24h
            )?),
            session_cookie: optional("SESSION_COOKIE").unwrap_or_else(|| "session".to_string()),
            subjects: Subjects {
                email: parse_subjects("EMAIL_SUBJECTS", &required("EMAIL_SUBJECTS")?)?,
                push: parse_subjects("PUSH_SUBJECTS", &required("PUSH_SUBJECTS")?)?,
            },
            settings: Settings {
                cache_ttl: Duration::from_secs(cache_ttl),
                otp_ttl: Duration::from_secs(otp_ttl),
            },
            email: EmailConfig {
                provider,
                api_key,
                from_address,
                from_name: optional("EMAIL_FROM_NAME")
                    .unwrap_or_else(|| "Notifications".to_string()),
            },
        })
    }
}
