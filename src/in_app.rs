//! In-app notifications.
//!
//! The simplest channel: no address, no OTP, no preferences. Every event whose
//! subject is in `IN_APP_SUBJECTS` is stored once per audience member, and the
//! user reads their own with `GET {namespace}/in-app`.
//!
//! Storage is the `in_app_notifications` table (see the migrations).

use actix_web::{HttpResponse, Responder, web};
use async_trait::async_trait;
use ferrumec::event::{EventError, EventMetaData, EventStream, Handler};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use std::sync::Arc;
use uuid::Uuid;
use viewset::{DefaultRepo, Entity, Filters, Repository};

use crate::resolver::IdResolver;

/// One stored notification, as returned by the endpoint.
#[derive(Entity, Clone, Serialize, Deserialize, FromRow)]
#[entity(create = "NewNotification")]
pub struct InAppNotification {
    #[entity(skip_create)]
    pub id: Uuid,
    #[entity(filterable)]
    pub user_id: Uuid,
    /// The event-stream subject it arrived on (e.g. `blogs.comment.created`).
    pub subject: String,
    /// The event as published (JSON); a plain string if it was not JSON.
    pub payload: Value,
    #[entity(skip_create)]
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NewNotification {
    pub user_id: Uuid,
    pub subject: String,
    /// The event as published (JSON); a plain string if it was not JSON.
    pub payload: Value,
}

type Repo = DefaultRepo<InAppNotification>;

#[derive(Clone)]
pub struct InApp {
    repo: Arc<Repo>,
}

struct OnEvent {
    repo: Arc<Repo>,
    id_resolver: Arc<dyn IdResolver>,
}

#[async_trait]
impl Handler for OnEvent {
    async fn handle(&self, subject: String, message: Vec<u8>) -> Result<(), EventError> {
        let message = match String::from_utf8(message) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "in-app: non-UTF-8 message on event stream");
                return Ok(());
            }
        };
        let payload: Value =
            serde_json::from_str(&message).unwrap_or_else(|_| Value::String(message.clone()));

        let metadata: EventMetaData = match serde_json::from_value(payload["metadata"].clone()) {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(error = %e, subject, "in-app: could not read event metadata");
                return Ok(());
            }
        };

        for id in metadata.audience {
            let user_id = match self.id_resolver.resolve(id).await {
                Ok(u) => u,
                Err(e) => {
                    tracing::error!(error = %e, subject, "in-app: invalid identifier; skipping");
                    continue;
                }
            };

            if let Err(e) = self
                .repo
                .create(NewNotification {
                    user_id,
                    subject: subject.clone(),
                    payload: payload.clone(),
                })
                .await
            {
                tracing::error!(error = %e, user = %user_id, subject, "in-app: failed to store notification");
            }
        }
        Ok(())
    }
}

impl InApp {
    /// Build the channel and subscribe it to `subjects` (or `>` when empty).
    pub async fn new(
        pool: PgPool,
        cf: impl ferrumec::CacheFactory,
        es: Arc<dyn EventStream>,
        id_resolver: Arc<dyn IdResolver>,
        subjects: Vec<String>,
    ) -> Self {
        let repo = Arc::new(DefaultRepo::new(pool, cf));
        let handler = Arc::new(OnEvent {
            repo: repo.clone(),
            id_resolver,
        });
        let patterns = if subjects.is_empty() {
            vec![">".to_string()]
        } else {
            subjects
        };
        for subject in patterns {
            if let Err(e) = es.clone().subscribe(subject.clone(), handler.clone()).await {
                tracing::error!(error = %e, subject, "in-app: error subscribing to event stream");
            }
        }
        Self { repo }
    }

    pub fn config(&self, cfg: &mut web::ServiceConfig, namespace: &str) {
        cfg.service(
            web::scope(namespace)
                .app_data(web::Data::from(self.repo.clone()))
                .route("", web::get().to(list)),
        );
    }
}

#[derive(Serialize)]
struct ListResponse {
    items: Vec<InAppNotification>,
    count: i64,
}

/// `GET /in-app` — the caller's notifications. Query parameters are viewset
/// filters; `user_id` is always forced to the session user.
async fn list(
    session: viewset::web::Identity,
    repo: web::Data<Repo>,
    query: web::Query<Filters>,
) -> impl Responder {
    let user_id = session.sub.to_string();
    let mut filters = query.into_inner();
    filters.insert("user_id".to_string(), user_id.clone());
    match repo.list(&filters).await {
        Ok((items, count)) => HttpResponse::Ok().json(ListResponse { items, count }),
        Err(e) => {
            tracing::error!(error = %e, user = %user_id, "in-app: failed to list notifications");
            HttpResponse::InternalServerError().finish()
        }
    }
}
