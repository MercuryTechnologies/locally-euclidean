//! Error handling: how errors are mapped to status codes and such.

use axum::http::StatusCode;

/// Type that means a meaningful status code.
pub trait ServiceError {
    fn status_code(&self) -> StatusCode;
}

/// DerivingVia for implementing IntoResponse for a service error based on
/// thiserror.
#[macro_export]
macro_rules! service_error {
    ($name:ident) => {
        impl ::axum::response::IntoResponse for $name {
            fn into_response(self) -> axum::response::Response {
                ::axum::response::IntoResponse::into_response((
                    $crate::errors::ServiceError::status_code(&self),
                    format!("{}", self),
                ))
            }
        }
    };
}
