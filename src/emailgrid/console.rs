use http::Response as HttpResponse;
use super::emailing::{EmailPayload, SendResult, Sender};
use async_trait::async_trait;

#[derive(Default, Clone, Copy)]
pub struct Console;

impl Console {
    pub fn new() -> Self {
        Console
    }
}

#[async_trait]
impl Sender for Console {
    async fn send(&self, payload: &EmailPayload) -> SendResult {
        let to = payload
            .to
            .iter()
            .map(|a| format!("{} <{}>", a.name, a.email))
            .collect::<Vec<_>>()
            .join(", ");

        println!("================ EMAIL (console) ================");
        println!("From:    {} <{}>", payload.sender.name, payload.sender.email);
        println!("To:      {to}");
        println!("Subject: {}", payload.subject);

        if payload.attachments.is_empty() {
            println!("Attachments: none");
        } else {
            println!("Attachments:");
            for a in &payload.attachments {
                println!("  - {} ({})", a.filename, a.path);
            }
        }

        println!("--------------------- HTML ---------------------");
        println!("{}", payload.html_content);
        println!("=================================================");

        let res = HttpResponse::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(r#"{"id":"console"}"#)?;

        Ok(reqwest::Response::from(res))
    }
}