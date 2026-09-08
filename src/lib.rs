use actix_web::{web, web::ServiceConfig};
use actixutils::{Identity, Validate};
use anyhow::Result;
use emailgrid::EmailingContext;
use mgk::{CreatePreference, GetAddress, IdResolver, Module as Mgk, Sender};
use push::{Config, NotificationRequest};
use sqlx::PgPool;
use sqlx::{FromRow, Pool, Postgres};
use std::env;
use std::sync::Arc;
use typed_eventbus::{EventStream, Identifier};
mod tagging;
use serde::{Deserialize, Serialize};
use viewset::{DefaultRepo, Entity, ViewSet};
struct Push(Config);
struct Console;
struct Email(EmailingContext);

#[derive(Entity, Serialize, Deserialize, Clone, FromRow)]
#[entity(create = "CreatePreference")]
struct EmailPreference {
    #[entity(skip_create)]
    id: Uuid,
    pub subject: String,
    pub address: String,
    pub user: String,
}

impl GetAddress for EmailPreference {
    fn get_address(&self) -> String {
        self.address.clone()
    }
}

type EmailRepo = DefaultRepo<EmailPreference>;

#[derive(Entity, Serialize, Deserialize, Clone, FromRow)]
#[entity(create = "CreatePreference")]
struct PushPreference {
    #[entity(skip_create)]
    id: Uuid,
    pub subject: String,
    pub address: String,
    pub user: String,
}

impl GetAddress for PushPreference {
    fn get_address(&self) -> String {
        self.address.clone()
    }
}

type PushRepo = DefaultRepo<PushPreference>;

#[derive(Entity, Serialize, Deserialize, Clone, FromRow)]
#[entity(create = "CreatePreference")]
struct ConsolePreference {
    #[entity(skip_create)]
    id: Uuid,
    pub subject: String,
    pub address: String,
    pub user: String,
}

impl GetAddress for ConsolePreference {
    fn get_address(&self) -> String {
        self.address.clone()
    }
}

type ConsoleRepo = DefaultRepo<ConsolePreference>;

pub struct MyIdResolver {
    pool: PgPool,
}

impl MyIdResolver {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl IdResolver for MyIdResolver {
    async fn resolve(&self, id: Identifier) -> Result<Uuid> {
        match id {
            Identifier::Uuid(id) => Ok(id),

            Identifier::Tag(tag) => {
                let user_id: Uuid =
                    sqlx::query_scalar("SELECT user_id FROM notification_tags WHERE tag = ?")
                        .bind(tag)
                        .fetch_one(&self.pool)
                        .await?;

                Ok(user_id)
            }
        }
    }
}

#[async_trait::async_trait]
impl Sender for Push {
    async fn send(&self, address: String, _subject: String, message: String) -> Result<()> {
        let notification = NotificationRequest {
            message,
            targets: vec![address],
        };
        self.0.push("push".to_string(), notification);
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

pub struct Module {
    emailer: Mgk<EmailRepo>,
    push_mgk: Mgk<PushRepo>,
    push_: Config,
    console: Mgk<ConsoleRepo>,
    pool: Pool<Postgres>,
}

use uuid::Uuid;

fn get_list(name: &str) -> Vec<String> {
    env::var(name)
        .expect(&format!("{name} not set"))
        .split(",")
        .map(|s| s.trim().to_string())
        .collect()
}

impl Module {
    pub async fn new(
        pool: Pool<Postgres>,
        emailer: EmailingContext,
        validator: Arc<dyn Validate<Identity>>,
        es: Arc<dyn EventStream>,
    ) -> anyhow::Result<Self> {
        let email_subjects = get_list("email.subjects");
        let push_subjects = get_list("push.subjects");
        let console_subjects = get_list("console.subjects");
        let idres = Arc::new(MyIdResolver::new(pool.clone()));
        let repo: ConsoleRepo = pool.clone().into();
        let console = Mgk::new(
            repo.into(),
            es.clone(),
            Arc::new(Console {}),
            idres.clone(),
            console_subjects,
        )
        .await?;
        let push_ = Config::new(validator).await;
        let repo: PushRepo = pool.clone().into();
        let push_mgk = Mgk::new(
            repo.into(),
            es.clone(),
            Arc::new(Push(push_.clone())),
            idres.clone(),
            push_subjects,
        )
        .await?;
        let repo: EmailRepo = pool.clone().into();
        let email = Mgk::new(
            repo.into(),
            es.clone(),
            Arc::new(Email(emailer)) as Arc<dyn Sender>,
            idres.clone(),
            email_subjects,
        )
        .await?;

        Ok(Self {
            emailer: email,
            push_mgk,
            push_,
            console,
            pool,
        })
    }

    pub fn config(&self, cfg: &mut ServiceConfig, namespace: &str) {
        cfg.app_data(web::Data::new(self.pool.clone())).service(
            web::scope(namespace)
                .configure(|cfg| {
                    tagging::create_tag_viewset(self.pool.clone()).configure(cfg, "tags")
                })
                .configure(|cfg| self.push_.config(cfg, "/ws"))
                .configure(|cfg| self.emailer.config(cfg, "/email"))
                .configure(|cfg| self.push_mgk.config(cfg, "/push"))
                .configure(|cfg| self.console.config(cfg, "/console")),
        );
    }
}
