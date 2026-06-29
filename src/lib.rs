use actix_web::{web,HttpResponse, web::ServiceConfig, post, Responder};
use actixutils::{Identity, Validate};
use anyhow::Result;
use emailgrid::EmailingContext;
use mgk::{IdResolver, Module as Mgk, Sender};
use push::{Config, NotificationRequest};
use sqlx::SqlitePool;
use sqlx::{Pool, Sqlite};
use std::env;
use std::sync::Arc;
use typed_eventbus::{EventStream, Identifier};

struct Push(Config);
struct Console;
struct Email(EmailingContext);

pub struct MyIdResolver {
    pool: SqlitePool,
}

impl MyIdResolver {
    pub fn new(pool: SqlitePool) -> Self {
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

#[derive(serde::Deserialize)]
pub struct RegisterTagRequest {
    pub tag: String,
    pub user_id: Uuid,
}

#[post("/tags")]
async fn register_tag(
    pool: web::Data<SqlitePool>,
    req: web::Json<RegisterTagRequest>,
) -> impl Responder {
    match sqlx::query(
        r#"
        INSERT INTO notification_tags(tag, user_id)
        VALUES(?, ?)
        ON CONFLICT(tag)
        DO UPDATE SET user_id = excluded.user_id
        "#,
    )
    .bind(&req.tag)
    .bind(req.user_id)
    .execute(pool.get_ref())
    .await{
        Ok(_)=>HttpResponse::Ok().finish(),
        Err(e)=>{
            tracing::error!("error in inserting tag: {e}");
            HttpResponse::InternalServerError().finish()
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

#[derive(Clone)]
pub struct Module {
    emailer: Mgk,
    push_mgk: Mgk,
    push_: Config,
    console: Mgk,
    pool: Pool<Sqlite>,
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
        pool: Pool<Sqlite>,
        emailer: EmailingContext,
        validator: Arc<dyn Validate<Identity>>,
        es: Arc<dyn EventStream>,
    ) -> anyhow::Result<Self> {
        let email_subjects = get_list("email.subjects");
        let push_subjects = get_list("push.subjects");
        let console_subjects = get_list("console.subjects");
        let idres = Arc::new(MyIdResolver::new(pool.clone()));
        let console = Mgk::new(
            pool.clone(),
            es.clone(),
            Arc::new(Console {}),
            idres.clone(),
            console_subjects,
        )
        .await?;
        let push_ = Config::new(validator).await;
        let push_mgk = Mgk::new(
            pool.clone(),
            es.clone(),
            Arc::new(Push(push_.clone())),
            idres.clone(),
            push_subjects,
        )
        .await?;
        let email = Mgk::new(
            pool.clone(),
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
        cfg.app_data(web::Data::new(self.pool.clone()))
            .service(register_tag)
            .service(
                web::scope(namespace)
                    .configure(|cfg| self.push_.config(cfg, "/ws"))
                    .configure(|cfg| self.emailer.config(cfg, "/email"))
                    .configure(|cfg| self.push_mgk.config(cfg, "/push"))
                    .configure(|cfg| self.console.config(cfg, "/console")),
            );
    }
}
