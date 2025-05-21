//! The Manifold API implementation.

use axum::{extract, routing::put};

use crate::AppState;

pub fn make_router() -> axum::Router<AppState> {
    axum::Router::new().route("/write/{*filename}", put(put_write_filename))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BucketArg {
    _bucket_name: String,
}

/// PUT `/v0/write/:filename?bucketName=:bucketName`
///
/// Streams the request body.
async fn put_write_filename(
    _state: extract::State<AppState>,
    _filename: extract::Path<String>,
    _query: extract::Query<BucketArg>,
    _request: extract::Request,
) {
    unimplemented!("todo!")
}
