use anyhow::Result;
use serde_json::Value;
use std::sync::Arc;
use tera::{Context, Tera};

use super::emailing::{Attachment, EmailAddress, EmailPayload, SendResult, Sender};

/// Builder for email content
#[derive(Clone)]
pub struct Builder {
    tera: Arc<Tera>,
}

impl Builder {
    /// Construct builder
    pub fn new() -> Result<Self> {
        // Load templates from ./templates directory
        let tera = Tera::new("templates/**/*")?;

        Ok(Self {
            tera: Arc::new(tera),
        })
    }

    /// Build email
    pub async fn build(&self, subject: String, message: String) -> Result<String> {
        // Parse JSON message
        let data: Value = serde_json::from_str(&message)?;

        // Prepare template name
        let template_name = format!("{}.html", subject);

        // Build Tera context
        let mut context = Context::new();
        context.extend(Context::from_serialize(&data)?);

        // Render template
        let rendered = self.tera.render(&template_name, &context)?;

        Ok(rendered)
    }
}

#[derive(Clone)]
pub struct EmailingContext {
    sender: Arc<dyn Sender>,
    builder: Builder,
    default_sender: EmailAddress,
    default_attachments: Vec<Attachment>,
}

impl EmailingContext {
    pub fn new(sender: Arc<dyn Sender>, default_sender: EmailAddress) -> Result<Self> {
        let builder = Builder::new()?;

        Ok(Self {
            sender,
            builder,
            default_sender,
            default_attachments: Vec::new(),
        })
    }

    /// Optional: allow configuring default attachments
    pub fn with_attachments(mut self, attachments: Vec<Attachment>) -> Self {
        self.default_attachments = attachments;
        self
    }

    /// High-level API: does everything
    pub async fn send(
        &self,
        email: String,
        subject: String,
        message: String,
    ) -> Result<SendResult> {
        // 1. Build (resolve email + render template)
        let html = self.builder.build(subject.clone(), message).await?;

        // 2. Construct payload
        let payload = EmailPayload {
            sender: self.default_sender.clone(),
            to: vec![EmailAddress {
                email,
                name: "".to_string(), // You might want to set this properly
            }],
            subject,
            html_content: html,
            attachments: self.default_attachments.clone(),
        };

        // 3. Send
        let result = self.sender.send(&payload).await;

        Ok(result)
    }
}
