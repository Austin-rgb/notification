//! Cookie-based session identity for the notification service.
//!
//! Sessions are created by an upstream auth service and stored under a UUID
//! cookie. This service only *reads* them (via [`RequiredSession`]) to obtain
//! the authenticated user's id.

use serde::{Deserialize, Serialize};

use uuid::Uuid;

/// Session payload stored server-side and referenced by the session cookie.
///
/// `user_id` is the authenticated subject (same role as JWT `sub`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthSession {
    pub user_id: Uuid,
}
