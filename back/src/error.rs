use std::{
    error::Error as StdError,
    fmt::{self, Display},
};

use actix_web::{HttpResponse, ResponseError};
use oauth2::http::StatusCode;
use serde::Serialize;

/// Generic JSON-based error response.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct JsonHttpError {
    #[serde(skip_serializing)]
    status: StatusCode,
    code: String,
    message: String,
}

impl JsonHttpError {
    pub fn new(status: StatusCode, code: impl ToString, message: impl ToString) -> Self {
        Self {
            status,
            code: code.to_string(),
            message: message.to_string(),
        }
    }

    pub fn unauthorized(code: impl ToString, message: impl ToString) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, code, message)
    }

    pub fn forbidden(code: impl ToString, message: impl ToString) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, message)
    }

    pub fn internal_server_error(code: impl ToString, message: impl ToString) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, code, message)
    }
}

impl Display for JsonHttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl ResponseError for JsonHttpError {
    fn status_code(&self) -> StatusCode {
        self.status
    }

    fn error_response(&self) -> HttpResponse {
        HttpResponse::build(self.status).json(self)
    }
}

impl StdError for JsonHttpError {}