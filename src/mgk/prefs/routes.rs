use crate::mgk::prefs::handlers::*;
use crate::mgk::{CreatePreference, GetAddress};
use actix_web::web;
use viewset::{Entity, Repository};

pub fn config<Repo: Repository + 'static>(cfg: &mut web::ServiceConfig)
where
    <<Repo as Repository>::Entity as Entity>::CreateDto: From<CreatePreference>,
    <Repo as Repository>::Entity: GetAddress,
{
    cfg
        // Preferences
        .route("/preferences/set", web::post().to(set_preference::<Repo>))
        .route(
            "/preferences/confirm",
            web::post().to(confirm_preference::<Repo>),
        )
        .route("/preferences/get", web::get().to(get_preference::<Repo>));
}
