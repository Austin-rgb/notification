use actixutils::extractors::Session;
use std::future::ready;
use actix_web::{
    Error, FromRequest, HttpMessage, HttpRequest, dev::Payload, error::ErrorBadRequest, web,
};
use futures_util::future::LocalBoxFuture;
pub struct ReadSession<T>(pub tokio::sync::RwLockReadGuard<'_, T>);

impl<T> FromRequest for ReadSession<T>{
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, payload: &mut Payload)->Self{
        Box::pin(async move {
        match req.extensions().get::<Session<T>>() {
            Some(session) => ready(Ok(session.read().await)),
            None => {
                tracing::error!("No session in request. Did you forget to wrap SessionMiddleware?");
                ready(Err(error::ErrorInternalServerError(
                    "Session requested without SessionMiddleware",
                )))
            }
        }
        })
    }
}