use actix_web::{get, Responder, web};
use actix_web::http::header;
use actix_web::web::ServiceConfig;

mod middleware;
pub mod utils;
mod lobby;
mod ws;

pub fn configure(svc: &mut ServiceConfig) {
    //API scope
    svc.service(api_scope().wrap(middleware::key_token::KeyTokens::new(vec![
        dotenvy::var("SERVER_TOKEN").unwrap_or_default(),
    ])));
    //WebService endpoint
    svc.service(
        web::resource("/ws")
            .route(web::get().to(ws::handler))
    );
    //Webservice at root if UPGRADE is set
    svc.service(
        web::resource("/")
            .guard(middleware::guard::header_insensitive(header::UPGRADE, "websocket"))
            .route(web::get().to(ws::handler))
    );
    //Root
    svc.service(root);
}

pub fn api_scope() -> actix_web::Scope {
    web::scope("/api")
        .service(test_api)
        .service(lobby::list)
        .service(lobby::game_types)
}

#[get("/")]
async fn root() -> impl Responder {
    utils::http_ok_utf8("🦀")
}

#[get("/test")]
pub async fn test_api() -> impl Responder {
    utils::http_ok_utf8("👀")
}
