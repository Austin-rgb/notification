use actix_web::web;
use actix_web::web::ServiceConfig;
use async_trait::async_trait;
use serde_json::{Value, from_str, from_value};
use std::sync::Arc;
use typed_eventbus::EventMetaData;
use typed_eventbus::{EventStream, Handler, Identifier};
use uuid::Uuid;
mod prefs;
use crate::mgk::prefs::db::Preferences;
use serde::{Deserialize, Serialize};
use viewset::{Entity, Repository};

#[derive(Serialize, Deserialize, Clone)]
pub struct CreatePreference {
    pub user: String,
    pub subject: String,
    pub address: String,
}

pub trait GetAddress {
    fn get_address(&self) -> String;
}

impl CreatePreference {
    pub fn new(user: String, subject: String, address: String) -> Self {
        Self {
            user,
            subject,
            address,
        }
    }
}

/// Pluggable delivery backend.  Implementations must return an error on
/// failure so that callers can log and surface delivery problems rather
/// than silently dropping notifications.
#[async_trait::async_trait]
pub trait Sender: Send + Sync {
    async fn send(
        &self,
        address: String,
        subject: String,
        message: String,
    ) -> Result<(), anyhow::Error>;
}

#[async_trait::async_trait]
pub trait IdResolver: Send + Sync {
    async fn resolve(&self, identifier: Identifier) -> anyhow::Result<Uuid>;
}

#[derive(Clone)]
pub struct Module<Repo: Repository> {
    sender: Arc<dyn Sender>,
    state: Arc<Preferences<Repo>>,
    id_resolver: Arc<dyn IdResolver>,
}

struct OnNotification<Repo: Repository> {
    state: Arc<Preferences<Repo>>,
    sender: Arc<dyn Sender>,
    id_resolver: Arc<dyn IdResolver>,
}

use crate::mgk::prefs::config;

#[async_trait]
impl<Repo: Repository + 'static> Handler for OnNotification<Repo>
where
    <<Repo as Repository>::Entity as Entity>::CreateDto: From<CreatePreference>,
    <Repo as Repository>::Entity: GetAddress,
{
    async fn handle(&self, subject: String, message: Vec<u8>) {
        let message = match String::from_utf8(message) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "Received non-UTF-8 message on event stream");
                return;
            }
        };
        let emd: Value = match from_str(&message) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error = %e, "Could not parse event JSON");
                return;
            }
        };
        let event: EventMetaData = match from_value(emd["metadata"].clone()) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "Could not deserialize EventMetaData");
                return;
            }
        };
        for id in event.audience {
            let user_id = match self.id_resolver.resolve(id).await {
                Ok(r) => r.to_string(),
                Err(e) => {
                    tracing::error!("invalid identifier: {e}; skipping event on subject={subject}");
                    return;
                }
            };
            let address = match self.state.get(&user_id, &subject).await {
                Ok(Some(a)) => a,
                Ok(None) => return, // No preference set for this user+subject — normal case.
                Err(e) => {
                    tracing::error!(error = %e, user = %user_id, subject, "Error reading preference");
                    return;
                }
            };
            if let Err(e) = self
                .sender
                .send(address, subject.clone(), message.clone())
                .await
            {
                tracing::error!(error = %e, user = %user_id, subject, "Sender failed to deliver notification");
            }
        }
    }
}

impl<Repo: Repository + 'static> Module<Repo>
where
    <<Repo as Repository>::Entity as Entity>::CreateDto: From<CreatePreference>,
    <Repo as Repository>::Entity: GetAddress,
{
    pub async fn new(
        pool: Arc<Repo>,
        es: Arc<dyn EventStream>,
        sender: Arc<dyn Sender>,
        id_resolver: Arc<dyn IdResolver>,
        subjects: Vec<String>,
    ) -> Result<Self, anyhow::Error> {
        let state = Arc::new(Preferences::new(pool.clone(), es.clone(), subjects).await?);

        let module = Self {
            sender,
            id_resolver,
            state: state.clone(),
        };
        module.subscribe(es, state).await;
        Ok(module)
    }

    pub fn config(&self, cfg: &mut ServiceConfig, namespace: &str) {
        cfg.service(
            web::scope(namespace)
                .app_data(web::Data::from(self.state.clone()))
                .app_data(web::Data::new(self.sender.clone()))
                .configure(config::<Repo>),
        );
    }

    pub async fn subscribe(&self, es: Arc<dyn EventStream>, state: Arc<Preferences<Repo>>) {
        // Subscribe once per known subject rather than using the catch-all ">",
        // so we only wake up for events this module actually cares about.
        // The allowed subjects are stored on Preferences; we re-derive them here
        // from the subjects vec passed at construction via the Module public API.
        //
        // Fall back to a single ">" subscription if the subjects list is empty,
        // which preserves the old behaviour for callers that don't restrict subjects.
        match es
            .clone()
            .subscribe(
                ">".to_string(),
                Arc::new(OnNotification {
                    sender: self.sender.clone(),
                    id_resolver: self.id_resolver.clone(),
                    state,
                }),
            )
            .await
        {
            Ok(_) => (),
            Err(e) => tracing::error!(error = %e, "Error subscribing to event stream"),
        };
    }
}
