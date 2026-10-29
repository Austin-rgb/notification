use crate::mgk::Email;
use crate::mgk::prefs::db::Preferences;
use crate::mgk::prefs::db::{PreferenceBatch, Token};
use actix_web::web;
use actix_web::{HttpResponse, Responder};
use serde::{Deserialize, Serialize};
use tracing::error;

// ---------------------------------------------------------------------------
// Preferences
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct PreferenceGetQuery {
    pub subject: String,
}

/// Response returned from POST /preferences/set so the client can pass the
/// nonce back to POST /preferences/confirm alongside the OTP.
#[derive(Serialize)]
struct SetResponse {
    nonce: String,
}

/// Body expected by POST /preferences/confirm.
#[derive(Deserialize)]
pub struct ConfirmBody {
    pub nonce: String,
    pub token: u32,
}

pub async fn set_preference(
    session: web::ReqData<viewset::web::Identity>,
    state: web::Data<Preferences>,
    email: web::Data<Email>,
    body: web::Json<PreferenceBatch>,
) -> impl Responder {
    let user_id = session.sub.to_string();
    let batch = body.into_inner();

    // Capture the address before we move the batch into `set`.
    let address = match batch.preferences.first() {
        Some(p) => p.address.clone(),
        None => return HttpResponse::BadRequest().body("preferences must not be empty"),
    };

    match state.set(&user_id, batch).await {
        Ok((nonce, otp)) => {
            // Send the OTP to the user's address out-of-band.
            // Log failures rather than silently swallowing them; the HTTP
            // response is still 200 because the pending entry was recorded
            // and the user may retry via /confirm within the TTL window.
            let message = serde_json::json!({ "otp": otp }).to_string();
            let result = email
                .send(address.clone(), "confirm address".to_owned(), message)
                .await;
            if let Err(e) = result {
                error!(
                    error = %e,
                    user = %user_id,
                    address = %address,
                    "Failed to deliver OTP; pending entry still recorded"
                );
            }
            HttpResponse::Ok().json(SetResponse { nonce })
        }
        Err(e) => {
            error!(
                error = %e,
                user = %user_id,
                "Failed to set user preference batch"
            );
            HttpResponse::Forbidden().body(e.to_string())
        }
    }
}

pub async fn confirm_preference(
    session: web::ReqData<viewset::web::Identity>,
    state: web::Data<Preferences>,
    body: web::Json<ConfirmBody>,
) -> impl Responder {
    let user_id = session.sub.to_string();
    let body = body.into_inner();
    let token = Token { token: body.token };
    match state.confirm(&user_id, &body.nonce, &token).await {
        Ok(_) => HttpResponse::Ok().finish(),
        Err(e) => {
            error!(
                error = %e,
                user = %user_id,
                "Failed to confirm user preference"
            );
            HttpResponse::InternalServerError().body(e.to_string())
        }
    }
}

pub async fn get_preference(
    session: web::ReqData<viewset::web::Identity>,
    state: web::Data<Preferences>,
    query: web::Query<PreferenceGetQuery>,
) -> impl Responder {
    let user_id = session.sub.to_string();
    match state.get(&user_id, &query.subject).await {
        Ok(Some(channel)) => HttpResponse::Ok().json(channel),
        Ok(None) => HttpResponse::NotFound().finish(),
        Err(e) => {
            error!(
                error = %e,
                user = %user_id,
                subject = %query.subject,
                "Failed to get user preference"
            );
            HttpResponse::InternalServerError().body(e.to_string())
        }
    }
}
