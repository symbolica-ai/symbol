//! A ready-made error response, so handlers can bail out with `?`.

use std::io;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::plain;
use crate::store::StoreError;

/// An error that is already the response the client will see.
///
/// Boxed so `Result<_, ApiError>` stays small.
pub struct ApiError(Box<Response>);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        *self.0
    }
}

impl From<Response> for ApiError {
    fn from(response: Response) -> Self {
        Self(Box::new(response))
    }
}

impl From<StoreError> for ApiError {
    fn from(err: StoreError) -> Self {
        err.into_response().into()
    }
}

impl From<io::Error> for ApiError {
    fn from(err: io::Error) -> Self {
        StoreError::Io(err).into()
    }
}

/// A request-validation message, answered as `400 Bad Request`.
impl From<&'static str> for ApiError {
    fn from(message: &'static str) -> Self {
        plain(StatusCode::BAD_REQUEST, message).into()
    }
}
