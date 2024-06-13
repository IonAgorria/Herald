use actix_web::{HttpResponse, Error, http};
use actix_web::dev::{forward_ready, Service, ServiceRequest, ServiceResponse, Transform};
use std::future::{Future, ready, Ready};
use actix_web::body::EitherBody;
use std::pin::Pin;

#[derive(Clone)]
pub struct KeyTokens {
    tokens: Vec<String>,
}

impl KeyTokens {
    pub fn new(tokens: Vec<String>) -> Self {
        Self {
            tokens,
        }
    }
}

impl<S, B> Transform<S, ServiceRequest> for KeyTokens
    where
        S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
        S::Future: 'static,
        B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type Transform = KeyTokenMiddleware<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(KeyTokenMiddleware {
            service,
            key_tokens: self.clone()
        }))
    }
}

pub struct KeyTokenMiddleware<S> {
    service: S,
    key_tokens: KeyTokens,
}

const AUTH_HEADER_BEARER: &'static str = "Bearer ";

impl<S, B> Service<ServiceRequest> for KeyTokenMiddleware<S>
    where
        S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
        S::Future: 'static,
        B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let auth_str = req.headers()
            .get(http::header::AUTHORIZATION)
            .map(|x| x.to_str());

        let is_authorized = match auth_str {
            Some(Ok(value)) if value.starts_with(AUTH_HEADER_BEARER) =>
            {
                let token = &value[AUTH_HEADER_BEARER.len()..];
                self.key_tokens.tokens
                    .iter()
                    .filter(|t| !t.is_empty())
                    .any(|t| token == t)
            }
            _ => false
        };

        if !is_authorized {
            let (request, _pl) = req.into_parts();
            let response = HttpResponse::Unauthorized()
                .force_close()
                .finish()
                .map_into_right_body();
            return Box::pin(async { Ok(ServiceResponse::new(request, response)) });
        }

        let res = self.service.call(req);
        Box::pin(async move {
            res.await.map(ServiceResponse::map_into_left_body).into()
        })
    }
}
