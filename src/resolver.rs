//! Turns an event audience entry ([`Identifier`]) into a user id.
//!
//! Shared by every channel (email, push, in-app).

use ferrumec::event::Identifier;
use sqlx::PgPool;
use uuid::Uuid;

use crate::Result;

#[async_trait::async_trait]
pub trait IdResolver: Send + Sync {
    async fn resolve(&self, identifier: Identifier) -> Result<Uuid>;
}

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
                    sqlx::query_scalar("SELECT user_id FROM notification_tags WHERE tag = $1")
                        .bind(tag)
                        .fetch_one(&self.pool)
                        .await?;

                Ok(user_id)
            }
        }
    }
}
