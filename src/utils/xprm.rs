use std::fmt;
use std::fmt::Display;
use actix_web::{
    http::StatusCode,
    ResponseError
};
use serde::Serialize;

pub fn serialize_xprm_or_json(xprm: bool, value: impl Serialize) -> Result<String, PayloadError> {
    if xprm {
        serde_kdlab_xprm::to_string(&value)
            .map_err(|e| PayloadError::SerializeXPRM(e))
    } else {
        serde_json::to_string(&value)
            .map_err(|e| PayloadError::SerializeJSON(e))
    }
}

#[derive(Debug)]
#[non_exhaustive]
pub enum PayloadError {
    SerializeXPRM(serde_kdlab_xprm::Error),
    SerializeJSON(serde_json::Error),
}

impl Display for PayloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        match self {
            PayloadError::SerializeXPRM(err) => err.fmt(formatter),
            PayloadError::SerializeJSON(err) => err.fmt(formatter),
        }
    }
}

impl ResponseError for PayloadError {
    fn status_code(&self) -> StatusCode {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

impl std::error::Error for PayloadError {}
