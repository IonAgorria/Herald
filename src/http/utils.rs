use actix_web::{HttpRequest, HttpResponse};
use actix_web::http::header;
use serde::Serialize;
use crate::utils::xprm::serialize_xprm_or_json;

pub fn http_ok_utf8(body: &str) -> HttpResponse {
    HttpResponse::Ok().body(format!(
        "<html><head><meta charset=\"UTF-8\"></head><body>{}</body></html>",
        body
    ))
}

pub fn http_ok_serialize(req: &HttpRequest, value: impl Serialize) -> HttpResponse {
    let is_xprm = is_xprm_accept(req.headers());
    match serialize_xprm_or_json(is_xprm, value) {
        Ok(body) => {
            HttpResponse::Ok()
                .content_type(
                    if is_xprm {
                        "application/xprm"
                    } else {
                        "application/json"
                    }
                )
                .body(body)
        }
        Err(err) => HttpResponse::from_error(err),
    }
}

pub fn is_xprm_accept(headers: &header::HeaderMap) -> bool {
    headers.get(header::ACCEPT)
    .map_or(false, |x| {
        x.to_str().unwrap_or("") == "application/xprm"
    })
}
