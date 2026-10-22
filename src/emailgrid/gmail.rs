use super::emailing::{EmailAddress, EmailPayload, SendResult, Sender};
use async_trait::async_trait;
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use reqwest::Client;
use serde::Deserialize;
use std::{
    env,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

type BoxErr = Box<dyn std::error::Error + Send + Sync>;

struct CachedToken {
    value: String,
    expires_at: Instant,
}

pub struct Gmail {
    client_id: String,
    client_secret: String,
    refresh_token: String,
    client: Client,
    token: Mutex<Option<CachedToken>>,
}

impl Gmail {
    pub fn new() -> Result<Self, env::VarError> {
        Ok(Gmail {
            client_id: env::var("GMAIL_CLIENT_ID")?,
            client_secret: env::var("GMAIL_CLIENT_SECRET")?,
            refresh_token: env::var("GMAIL_REFRESH_TOKEN")?,
            client: Client::new(),
            token: Mutex::new(None),
        })
    }

    async fn access_token(&self) -> Result<String, BoxErr> {
        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: String,
            expires_in: u64,
        }

        let mut guard = self.token.lock().await;
        if let Some(t) = guard.as_ref() {
            if t.expires_at > Instant::now() {
                return Ok(t.value.clone());
            }
        }

        let res = self
            .client
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("refresh_token", self.refresh_token.as_str()),
                ("grant_type", "refresh_token"),
            ])
            .send()
            .await?;

        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            return Err(format!("gmail token refresh failed ({status}): {body}").into());
        }

        let tr: TokenResponse = res.json().await?;
        // Refresh a minute early to avoid using a token that expires mid-request.
        let ttl = tr.expires_in.saturating_sub(60);
        *guard = Some(CachedToken {
            value: tr.access_token.clone(),
            expires_at: Instant::now() + Duration::from_secs(ttl),
        });
        Ok(tr.access_token)
    }

    /// `path` may be an http(s) URL (like Resend) or a local file path.
    async fn load_attachment(&self, path: &str) -> Result<Vec<u8>, BoxErr> {
        if path.starts_with("http://") || path.starts_with("https://") {
            let res = self.client.get(path).send().await?.error_for_status()?;
            Ok(res.bytes().await?.to_vec())
        } else {
            Ok(tokio::fs::read(path).await?)
        }
    }

    async fn build_mime(&self, p: &EmailPayload) -> Result<String, BoxErr> {
        for a in std::iter::once(&p.sender).chain(p.to.iter()) {
            if a.email.contains(['\r', '\n']) || a.name.contains(['\r', '\n']) {
                return Err("invalid characters in email address or name".into());
            }
        }

        let from = format_addr(&p.sender);
        let to = p.to.iter().map(format_addr).collect::<Vec<_>>().join(", ");
        let boundary = format!(
            "=_gmail_{}",
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        );

        let mut m = String::new();
        m.push_str("MIME-Version: 1.0\r\n");
        m.push_str(&format!("From: {from}\r\n"));
        m.push_str(&format!("To: {to}\r\n"));
        m.push_str(&format!("Subject: {}\r\n", encode_word(&p.subject)));
        m.push_str(&format!(
            "Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n"
        ));

        // HTML body
        m.push_str(&format!("--{boundary}\r\n"));
        m.push_str("Content-Type: text/html; charset=UTF-8\r\n");
        m.push_str("Content-Transfer-Encoding: base64\r\n\r\n");
        m.push_str(&wrap76(&STANDARD.encode(p.html_content.as_bytes())));
        m.push_str("\r\n");

        // Attachments
        for a in &p.attachments {
            let data = self.load_attachment(&a.path).await?;
            let name = encode_word(&a.filename);
            m.push_str(&format!("--{boundary}\r\n"));
            m.push_str(&format!(
                "Content-Type: application/octet-stream; name=\"{name}\"\r\n"
            ));
            m.push_str(&format!(
                "Content-Disposition: attachment; filename=\"{name}\"\r\n"
            ));
            m.push_str("Content-Transfer-Encoding: base64\r\n\r\n");
            m.push_str(&wrap76(&STANDARD.encode(&data)));
            m.push_str("\r\n");
        }

        m.push_str(&format!("--{boundary}--\r\n"));
        Ok(m)
    }
}

fn format_addr(a: &EmailAddress) -> String {
    if a.name.is_empty() {
        a.email.clone()
    } else {
        format!("{} <{}>", encode_word(&a.name), a.email)
    }
}

/// RFC 2047 encoded-word; plain ASCII without specials passes through.
fn encode_word(s: &str) -> String {
    let plain =
        s.is_ascii() && !s.contains(['\r', '\n', '"', '<', '>', ',', ';', '(', ')', '=', '?']);
    if plain {
        s.to_string()
    } else {
        format!("=?UTF-8?B?{}?=", STANDARD.encode(s.as_bytes()))
    }
}

fn wrap76(s: &str) -> String {
    s.as_bytes()
        .chunks(76)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join("\r\n")
}

#[async_trait]
impl Sender for Gmail {
    async fn send(&self, payload: &EmailPayload) -> SendResult {
        let token = self.access_token().await?;
        let mime = self.build_mime(payload).await?;
        let raw = URL_SAFE_NO_PAD.encode(mime.as_bytes());

        let res = self
            .client
            .post("https://gmail.googleapis.com/gmail/v1/users/me/messages/send")
            .bearer_auth(token)
            .json(&serde_json::json!({ "raw": raw }))
            .send()
            .await?;

        Ok(res)
    }
}
