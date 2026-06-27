use actix_web::web;
use actix_web::web::ServiceConfig;
use actixutils::{Identity, Validate};
use anyhow::Result;
use emailgrid::EmailingContext;
use mgk::{Module as Mgk, Sender};
use push::Config;
use serde::{Deserialize, Serialize};
use sqlx::{Pool, Sqlite};
use std::env;
use std::sync::Arc;
use typed_eventbus::EventStream;

struct Push(Config);
struct Console;
struct Email(EmailingContext);

#[async_trait::async_trait]
impl Sender for Push {
    async fn send(&self, _address: String, _subject: String, message: String) -> Result<()> {
        self.0.push("push".to_string(), message);
        Ok(())
    }
    fn get_name(&self) -> std::string::String {
        "push".to_string()
    }
}

#[async_trait::async_trait]
impl Sender for Email {
    async fn send(&self, address: String, subject: String, message: String) -> Result<()> {
        let _ = self.0.send(address, subject, message).await;
        Ok(())
    }
    fn get_name(&self) -> std::string::String {
        "email".to_string()
    }
}

#[async_trait::async_trait]
impl Sender for Console {
    async fn send(&self, address: String, subject: String, message: String) -> Result<()> {
        println!("message sent: address = {address}, subject = {subject}, message = {message}");
        Ok(())
    }

    fn get_name(&self) -> std::string::String {
        "console".to_string()
    }
}

#[derive(Clone)]
pub struct Module {
    emailer: Mgk,
    push_mgk: Mgk,
    push_: Config,
    console: Mgk,
}
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub event_id: Uuid,
    pub event_version: String,
    pub occurred_at: DateTime<Utc>,
    pub producer: String,
    pub correlation_id: Option<Uuid>,
    pub trace_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
}

fn get_list(name: &str) -> Vec<String> {
    env::var(name)
        .expect(&format!("{name} not set"))
        .split(",")
        .map(|s| s.trim().to_string())
        .collect()
}

impl Module {
    pub async fn new(
        pool: Pool<Sqlite>,
        emailer: EmailingContext,
        validator: Arc<dyn Validate<Identity>>,
        es: Arc<dyn EventStream>,
    ) -> anyhow::Result<Self> {
        let email_subjects = get_list("email.subjects");
        let push_subjects = get_list("push.subjects");
        let console_subjects = get_list("console.subjects");
        let console = Mgk::new(
            pool.clone(),
            es.clone(),
            Arc::new(Console {}),
            console_subjects,
        )
        .await?;
        let push_ = Config::new(validator).await;
        let push_mgk = Mgk::new(
            pool.clone(),
            es.clone(),
            Arc::new(Push(push_.clone())),
            push_subjects,
        )
        .await?;
        let email = Mgk::new(
            pool.clone(),
            es.clone(),
            Arc::new(Email(emailer)) as Arc<dyn Sender>,
            email_subjects,
        )
        .await?;

        Ok(Self {
            emailer: email,
            push_mgk,
            push_,
            console,
        })
    }

    pub fn config(&self, cfg: &mut ServiceConfig, namespace: &str) {
        cfg.service(
            web::scope(namespace)
                .configure(|cfg| self.push_.config(cfg, "/ws"))
                .configure(|cfg| self.emailer.config(cfg, "/email"))
                .configure(|cfg| self.push_mgk.config(cfg, "/push"))
                .configure(|cfg| self.console.config(cfg, "/console")),
        );
    }
}
