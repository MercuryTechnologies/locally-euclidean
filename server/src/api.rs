//! The Manifold API implementation.

use axum::{
    extract,
    routing::{post, put},
};

use crate::AppState;

pub fn make_router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/write/{*filename}", put(put_write_filename))
        .route("/append/{*filename}", post(post_append_filename))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriteFilenameArgs {
    _bucket_name: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppendFilenameArgs {
    _bucket_name: String,
    _write_offset: u64,
}

/// PUT `/v0/write/:filename?bucketName=:bucketName`
///
/// Streams the request body.
async fn put_write_filename(
    _state: extract::State<AppState>,
    _filename: extract::Path<String>,
    _query: extract::Query<WriteFilenameArgs>,
    _request: extract::Request,
) {
    unimplemented!("todo!")
}

/// POST `/v0/append/:filename?bucketName=:bucketName&writeOffset=:writeOffset`
///
/// Streams the request body.
async fn post_append_filename(
    _state: extract::State<AppState>,
    _filename: extract::Path<String>,
    _query: extract::Query<WriteFilenameArgs>,
    _request: extract::Request,
) {
    // TODO(jadel): implement. Needs to do the writes to the file in a
    // tokio::spawn task so that they cannot get partially completed by the
    // sender.
    //
    // FIXME(jadel): is it semantically acceptable to stream the request body?
    // Technically the request could partially finish before dying, and either we have to
    // fully buffer the request body in memory (maybe fine) or write it to the
    // final file as we go (which means that we don't have atomicity in the
    // case of surprise-disconnects).
    //
    // A design change that would fix this is to use copy_file_range to
    // leverage modern CoW filesystems such that appending to a file is done by
    // appending to a buffer then renaming over the original file. However,
    // this breaks inode based locking schemes and would require locking on the
    // basis of canonicalized filename. In short: also eww edge cases!
    unimplemented!("todo!")
}
