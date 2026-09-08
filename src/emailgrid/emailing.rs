use async_trait::async_trait;
use reqwest::Client;
use reqwest::Response;
use serde::Serialize;
use std::env;

#[derive(Serialize, Clone)]
pub struct EmailAddress {
    pub email: String,
    pub name: String,
}

#[derive(Serialize, Clone)]
pub struct Attachment {
    pub path: String,
    pub filename: String,
}

#[derive(Serialize)]
pub struct EmailPayload {
    pub sender: EmailAddress,
    pub to: Vec<EmailAddress>,
    pub subject: String,
    #[serde(rename = "htmlContent")]
    pub html_content: String,
    pub attachments: Vec<Attachment>,
}

pub type SendResult = Result<Response, Box<dyn std::error::Error + Send + Sync>>;

#[async_trait]
pub trait Sender: Send + Sync {
    async fn send(&self, payload: &EmailPayload) -> SendResult;
}

pub struct Brevo(pub String);

impl Brevo {
    pub fn new() -> Result<Self, env::VarError> {
        let api_key = env::var("BREVO_API_KEY")?;
        Ok(Brevo(api_key))
    }
}

#[async_trait]
impl Sender for Brevo {
    async fn send(&self, payload: &EmailPayload) -> SendResult {
        let client = Client::new();
        let res = client
            .post("https://api.brevo.com/v3/smtp/email")
            .header("accept", "application/json")
            .header("api-key", self.0.clone())
            .json(payload)
            .send()
            .await?;
        Ok(res)
    }
}

pub struct Resend(pub String);

impl Resend {
    pub fn new() -> Result<Self, env::VarError> {
        let api_key = env::var("RESEND_API_KEY")?;
        Ok(Resend(api_key))
    }
}

#[async_trait]
impl Sender for Resend {
    async fn send(&self, payload: &EmailPayload) -> SendResult {
        #[derive(Serialize)]
        struct ResendAttachment<'a> {
            filename: &'a String,
            path: &'a String,
        }

        #[derive(Serialize)]
        struct ResendPayload<'a> {
            from: String,
            to: Vec<&'a String>,
            subject: &'a String,
            html: &'a String,
            attachments: Vec<ResendAttachment<'a>>,
        }

        let client = Client::new();
        let key = self.0.clone();

        let rp = ResendPayload {
            from: format!("{} <{}>", payload.sender.name, payload.sender.email),
            to: payload.to.iter().map(|t| &t.email).collect(),
            subject: &payload.subject,
            html: &payload.html_content,
            attachments: payload
                .attachments
                .iter()
                .map(|a| ResendAttachment {
                    filename: &a.filename,
                    path: &a.path,
                })
                .collect(),
        };

        let res = client
            .post("https://api.resend.com/emails")
            .header("Authorization", format!("Bearer {}", key))
            .header("Content-Type", "application/json")
            .json(&rp)
            .send()
            .await?;

        Ok(res)
    }
}
