use crate::mgk::prefs::handlers::*;
use actix_web::web;

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg
        // Preferences
        .route("/preferences/set", web::post().to(set_preference))
        .route("/preferences/confirm", web::post().to(confirm_preference))
        .route("/preferences/get", web::get().to(get_preference));
}
