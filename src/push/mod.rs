//! Push notifications over websocket.
//!
//! Works like the in-app channel: no address, no OTP, no preferences. Every
//! event whose subject is in `PUSH_SUBJECTS` is forwarded to each resolved
//! audience member who currently has a websocket open on `GET {namespace}/ws/`
//! (the websocket is keyed by the session user id). Users who are not connected
//! simply miss the push.

mod ws;

use actix::{Actor, Addr};
use actix_web::web::{self, ServiceConfig};
use async_trait::async_trait;
use ferrumec::event::{EventError, EventMetaData, EventStream, Handler};
use serde_json::Value;
use std::sync::Arc;

use crate::resolver::IdResolver;
use ws::{ChatServer, MessageOnTrans, Service, deliver_message, ws_route};

#[derive(Clone)]
pub struct Push {
    state: Service,
}

struct OnEvent {
    chat_server: Addr<ChatServer>,
    id_resolver: Arc<dyn IdResolver>,
}

#[async_trait]
impl Handler for OnEvent {
    async fn handle(&self, subject: String, message: Vec<u8>) -> Result<(), EventError> {
        let message = match String::from_utf8(message) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "push: non-UTF-8 message on event stream");
                return Ok(());
            }
        };
        let payload: Value =
            serde_json::from_str(&message).unwrap_or_else(|_| Value::String(message.clone()));

        let metadata: EventMetaData = match serde_json::from_value(payload["metadata"].clone()) {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(error = %e, subject, "push: could not read event metadata");
                return Ok(());
            }
        };

        for id in metadata.audience {
            let user_id = match self.id_resolver.resolve(id).await {
                Ok(u) => u,
                Err(e) => {
                    tracing::error!(error = %e, subject, "push: invalid identifier; skipping");
                    continue;
                }
            };

            deliver_message(
                &MessageOnTrans::new("push".to_string(), message.clone()),
                vec![user_id.to_string()],
                self.chat_server.clone(),
            );
        }
        Ok(())
    }
}

impl Push {
    /// Start the websocket server and subscribe it to `subjects` (or `>` when
    /// empty). Must be called from inside a running actix system.
    pub async fn new(
        es: Arc<dyn EventStream>,
        id_resolver: Arc<dyn IdResolver>,
        subjects: Vec<String>,
    ) -> Self {
        let chat_server = ChatServer::new().start();
        let handler = Arc::new(OnEvent {
            chat_server: chat_server.clone(),
            id_resolver,
        });
        let patterns = if subjects.is_empty() {
            vec![">".to_string()]
        } else {
            subjects
        };
        for subject in patterns {
            if let Err(e) = es.clone().subscribe(subject.clone(), handler.clone()).await {
                tracing::error!(error = %e, subject, "push: error subscribing to event stream");
            }
        }
        Self {
            state: Service { chat_server },
        }
    }

    pub fn config(&self, cfg: &mut ServiceConfig, namespace: &str) {
        cfg.service(
            web::scope(namespace)
                .app_data(web::Data::new(self.state.clone()))
                .service(ws_route),
        );
    }
}
