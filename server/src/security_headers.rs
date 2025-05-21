//! Adds various security headers to make this not as fun to abuse for XSS.
//!
//! It's still kind of abusable for XSS though since we necessarily allow
//! uploading HTML.
//!
//! FIXME(jadel): find more fun headers to set
use axum::{
    http::{HeaderValue, header},
    middleware::Next,
    response::Response,
};
use tower_http::cors::CorsLayer;

const DEFAULT_HEADERS: [(header::HeaderName, HeaderValue); 4] = [
    (
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    ),
    (header::X_FRAME_OPTIONS, HeaderValue::from_static("deny")),
    (
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static(""),
    ),
    (
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    ),
];

pub(crate) async fn headers(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    for (name, value) in DEFAULT_HEADERS {
        headers.entry(name).or_insert(value);
    }
    response
}

pub(crate) fn cors() -> CorsLayer {
    CorsLayer::new()
}

// FIXME(jadel): I gave up trying to combine these into one combined middleware
// because async rust is pain.
