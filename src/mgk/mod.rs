//! MGK: the email channel.
//!
//! Unlike in-app and push, email needs an address, so this module owns the
//! preference flow (set -> OTP by email -> confirm) and only delivers to
//! users who confirmed an address for the event's subject.

use actix_web::web;
use actix_web::web::ServiceConfig;
use async_trait::async_trait;
use ferrumec::cache::CacheFactory;
use ferrumec::event::{EventError, EventMetaData, EventStream, Handler};
use serde::{Deserialize, Serialize};
use serde_json::{Value, from_str, from_value};
use sqlx::{FromRow, PgPool};
use std::sync::Arc;
use uuid::Uuid;
use viewset::{DefaultRepo, Entity};

use crate::Result;
use crate::config::Settings;
use crate::emailgrid::EmailingContext;
use crate::resolver::IdResolver;

mod prefs;
use prefs::config;
use prefs::db::Preferences;

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

#[derive(Serialize, Deserialize, Clone)]
pub struct CreatePreference {
    /// Owner of the preference. (Not called `user`: reserved word in PostgreSQL.)
    pub user_id: String,
    pub subject: String,
    pub address: String,
}

type EmailRepo = DefaultRepo<EmailPreference>;

/// Email delivery on top of [`EmailingContext`].
pub struct Email(EmailingContext);

impl Email {
    /// Sends one email. Returns an error on any failure (template, transport or
    /// non-2xx from the provider) so callers can log it instead of silently
    /// dropping the email.
    pub async fn send(&self, address: String, subject: String, message: String) -> Result<()> {
        // The template builder needs a JSON object. Anything else (e.g. a bare
        // value) is exposed to the template as `{{ value }}`.
        let message = match serde_json::from_str::<Value>(&message) {
            Ok(v) if v.is_object() => message,
            Ok(v) => serde_json::json!({ "value": v }).to_string(),
            Err(_) => serde_json::json!({ "value": message }).to_string(),
        };
        // Template / build errors surface here...
        let response = self.0.send(address, subject, message).await?;
        // ...and so do transport errors and non-2xx answers from the provider.
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

#[derive(Clone)]
pub struct Module {
    email: Arc<Email>,
    state: Arc<Preferences>,
}

struct OnNotification {
    state: Arc<Preferences>,
    email: Arc<Email>,
    id_resolver: Arc<dyn IdResolver>,
}

#[async_trait]
impl Handler for OnNotification {
    async fn handle(&self, subject: String, message: Vec<u8>) -> core::result::Result<(), EventError> {
        let message = match String::from_utf8(message) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "Received non-UTF-8 message on event stream");
                return Ok(());
            }
        };
        let emd: Value = match from_str(&message) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error = %e, "Could not parse event JSON");
                return Ok(());
            }
        };
        let event: EventMetaData = match from_value(emd["metadata"].clone()) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "Could not deserialize EventMetaData");
                return Ok(());
            }
        };
        for id in event.audience {
            let user_id = match self.id_resolver.resolve(id).await {
                Ok(r) => r.to_string(),
                Err(e) => {
                    tracing::error!("invalid identifier: {e}; skipping event on subject={subject}");
                    continue;
                }
            };
            let address = match self.state.get(&user_id, &subject).await {
                Ok(Some(a)) => a,
                // No preference for this user+subject is the normal case: skip this
                // audience member only, the rest of the audience still gets notified.
                Ok(None) => continue,
                Err(e) => {
                    tracing::error!(error = %e, user = %user_id, subject, "Error reading preference");
                    continue;
                }
            };
            if let Err(e) = self
                .email
                .send(address, subject.clone(), message.clone())
                .await
            {
                tracing::error!(error = %e, user = %user_id, subject, "Failed to deliver email notification");
            }
        }
        Ok(())
    }
}

impl Module {
    pub async fn new(
        pool: PgPool,
        es: Arc<dyn EventStream>,
        emailing: EmailingContext,
        id_resolver: Arc<dyn IdResolver>,
        subjects: Vec<String>,
        kv: impl CacheFactory,
        settings: Settings,
    ) -> Result<Self> {
        let repo = Arc::new(EmailRepo::new(pool, kv.clone()));
        let state = Arc::new(Preferences::new(repo, es.clone(), subjects.clone(), kv, settings).await?);

        let module = Self {
            email: Arc::new(Email(emailing)),
            state: state.clone(),
        };
        module.subscribe(es, state, id_resolver, subjects).await;
        Ok(module)
    }

    pub fn config(&self, cfg: &mut ServiceConfig, namespace: &str) {
        cfg.service(
            web::scope(namespace)
                .app_data(web::Data::from(self.state.clone()))
                .app_data(web::Data::from(self.email.clone()))
                .configure(config),
        );
    }

    async fn subscribe(
        &self,
        es: Arc<dyn EventStream>,
        state: Arc<Preferences>,
        id_resolver: Arc<dyn IdResolver>,
        subjects: Vec<String>,
    ) {
        // Subscribe once per known subject rather than using the catch-all ">",
        // so we only wake up for events this module actually cares about.
        // An empty list falls back to a single ">" subscription.
        let handler = Arc::new(OnNotification {
            email: self.email.clone(),
            id_resolver,
            state,
        });

        let patterns: Vec<String> = if subjects.is_empty() {
            vec![">".to_string()]
        } else {
            subjects
        };

        for subject in patterns {
            if let Err(e) = es.clone().subscribe(subject.clone(), handler.clone()).await {
                tracing::error!(error = %e, subject, "Error subscribing to event stream");
            }
        }
    }
}
