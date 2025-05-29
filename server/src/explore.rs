//! `/explore/:bucketName/:filename` endpoint, for viewing files in the
//! browser.
//!
//! FIXME(jadel): does this want to be "just serve the files as http with no
//! UI" or does this want to be "with UI"?
use std::time::SystemTime;

use axum::{
    body::Body,
    extract,
    http::{
        HeaderValue, Response,
        header::{CONTENT_LENGTH, CONTENT_TYPE, LAST_MODIFIED},
    },
    routing::get,
};
use storage::{Bucket, FileHandleOps, StorageBackend};
use tokio::io::AsyncSeekExt;
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;

use crate::{AppState, api::ApiError};

pub fn make_router() -> axum::Router<AppState> {
    axum::Router::new().route("/{bucket}/{*filename}", get(get_explore))
}

// FIXME(jadel): should handle HEAD requests, if-modified-since, and range requests properly
// Sadly there's no really trivial way to simply yoink such code from
// tower-http.

/// This is arbitrarily chosen as 64KiB.
const BUFFER_SIZE: u64 = 64 * 1024;

#[tracing::instrument(level = "info", skip(state))]
async fn get_explore(
    state: extract::State<AppState>,
    extract::Path((bucket, filename)): extract::Path<(String, String)>,
) -> Result<Response<Body>, ApiError> {
    let bucket = state
        .store
        .bucket(&bucket)
        .await
        .map_err(|e| ApiError::from_bucket_open_error(&bucket, e))?;

    let mut file = bucket
        .file(&filename)
        .await
        .map_err(|e| ApiError::from_bucket_file_open_error(&filename, e))?;

    // We know the file exists now, so we expect to return a 200 and stream the
    // response.
    let length = file
        .seek(std::io::SeekFrom::End(0))
        .await
        .map_err(ApiError::internal)?;
    file.seek(std::io::SeekFrom::Start(0))
        .await
        .map_err(ApiError::internal)?;
    let metadata = file.metadata().await.map_err(ApiError::internal)?;

    let mut response = Response::new(Body::from_stream(
        ReaderStream::with_capacity(file, BUFFER_SIZE as usize)
            .map(|data| data.map_err(ApiError::internal)),
    ));
    {
        let headers = response.headers_mut();
        headers.insert(
            CONTENT_LENGTH,
            HeaderValue::try_from(length.to_string())
                .expect("no invalid chars in number to_string"),
        );

        // FIXME(jadel): Real content-type from the storage backend
        // https://linear.app/mercury/issue/DUX-3499/correctly-handle-content-type-of-uploads
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );

        headers.insert(
            LAST_MODIFIED,
            HeaderValue::try_from(
                httpdate::HttpDate::from(SystemTime::from(metadata.last_modified_at)).to_string(),
            )
            .expect("httpdate does not give out-of-range chars"),
        );
    }

    Ok(response)
}
