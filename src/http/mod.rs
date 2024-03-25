use actix_web::{get, Responder, web};
use actix_web::web::ServiceConfig;
use middleware::key_token::KeyTokens;

mod middleware;
pub mod utils;
mod lobby;
mod ws;

pub fn scope() -> actix_web::Scope {
    web::scope("/api")
        .service(test_api)
        .service(lobby::list)
        .service(lobby::game_types)
}

pub fn configure(svc: &mut ServiceConfig) {
    svc.service(scope().wrap(KeyTokens::new(vec![
        dotenvy::var("SERVER_TOKEN").unwrap_or_default(),
    ])));
    svc.service(ws::service);
}

#[get("/test")]
pub async fn test_api() -> impl Responder {
    utils::http_ok_utf8("👀")
}
