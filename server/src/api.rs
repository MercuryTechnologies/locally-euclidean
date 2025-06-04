//! The Manifold API implementation.

use std::io::{ErrorKind, SeekFrom};

use axum::{
    body::{Body, Bytes},
    extract,
    http::StatusCode,
    response::Response,
    routing::{get, post, put},
};
use http_body_util::BodyExt;
use storage::{Bucket, FileCreateError, FileHandleOps, FileOpenError, StorageBackend};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt};
use tokio_stream::{Stream, StreamExt};

use crate::{AppState, errors::ServiceError, explore, service_error};

pub fn make_router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/write/{*filename}", put(put_write_filename))
        .route("/append/{*filename}", post(post_append_filename))
}

/// Makes the router for `/v1`, which is buck2-specific functionality.
///
/// See: <https://github.com/facebook/buck2/pull/770>
pub fn make_buck2_logs_router() -> axum::Router<AppState> {
    axum::Router::new().route("/get/{filename}", get(get_buck2_log))
}

#[derive(serde::Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct WriteFilenameArgs {
    bucket_name: String,
}

#[derive(serde::Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct AppendFilenameArgs {
    bucket_name: String,
    write_offset: u64,
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum ApiError {
    #[error("Bucket does not exist: {0:?}")]
    BucketDoesNotExist(String),
    #[error("Invalid bucket name: {0:?}")]
    InvalidBucketName(String),
    #[error("File does not exist: {0:?}")]
    FileDoesNotExist(String),
    #[error("Invalid file name: {0:?}")]
    InvalidFileName(String),
    #[error("File already exists with conflicting content")]
    FileExistsWithConflictingContent,
    #[error("Internal error: {0}")]
    InternalError(axum::BoxError),
}

impl ServiceError for ApiError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::BucketDoesNotExist(_) => StatusCode::NOT_FOUND,
            Self::InvalidBucketName(_) => StatusCode::BAD_REQUEST,
            Self::FileDoesNotExist(_) => StatusCode::NOT_FOUND,
            Self::InvalidFileName(_) => StatusCode::BAD_REQUEST,
            Self::FileExistsWithConflictingContent => StatusCode::CONFLICT,
            Self::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

service_error!(ApiError);

impl ApiError {
    // FIXME(jadel): this really is bad error reporting and probably should be
    // replaced with miette or something which allows tracking where something
    // was wrapped.
    pub(crate) fn internal(err: impl Into<axum::BoxError>) -> Self {
        Self::InternalError(err.into())
    }

    // This is not a From impl since FileOpenError can occur for more than just
    // opening a bucket.

    pub(crate) fn from_bucket_open_error(name: &str, err: FileOpenError) -> ApiError {
        match err {
            FileOpenError::DoesNotExist => ApiError::BucketDoesNotExist(name.to_owned()),
            FileOpenError::InvalidName => ApiError::InvalidBucketName(name.to_owned()),
            e => Self::internal(e),
        }
    }

    pub(crate) fn from_bucket_file_open_error(name: &str, err: FileOpenError) -> ApiError {
        match err {
            FileOpenError::DoesNotExist => ApiError::FileDoesNotExist(name.to_owned()),
            FileOpenError::InvalidName => ApiError::InvalidFileName(name.to_owned()),
            e => Self::internal(e),
        }
    }
}

/// Result of matching ranges of data.
#[derive(Debug, PartialEq, Eq)]
enum RangeMatch {
    Matches,
    LengthMismatch,
    DataMismatch,
}

/// Checks that a range of data from a stream matches in length and content.
#[tracing::instrument(level = "trace", skip(stream, file))]
async fn check_range_matches(
    mut stream: impl Stream<Item = Result<Bytes, axum::Error>> + Unpin,
    start_position: u64,
    mut file: impl AsyncSeek + AsyncRead + Unpin,
) -> Result<RangeMatch, axum::BoxError> {
    // This might seek off the end of the file. However, that's okay.
    file.seek(SeekFrom::Start(start_position)).await?;

    let mut buf = Vec::with_capacity(4096);
    while let Some(data) = stream.try_next().await? {
        buf.resize(data.len(), 0);
        let res = file.read_exact(&mut buf).await;
        if let Err(ref err) = res {
            if err.kind() == ErrorKind::UnexpectedEof {
                return Ok(RangeMatch::LengthMismatch);
            }
        }
        res?;

        if buf != data {
            return Ok(RangeMatch::DataMismatch);
        }
    }

    match file.read_u8().await {
        // Any data left in the file means that there is a length mismatch.
        Ok(_) => Ok(RangeMatch::LengthMismatch),
        // We expect an EOF here.
        Err(err) if err.kind() == ErrorKind::UnexpectedEof => Ok(RangeMatch::Matches),
        Err(err) => Err(err.into()),
    }
}

/// PUT `/v0/write/:filename?bucketName=:bucketName`
///
/// Streams the request body.
#[tracing::instrument(level = "info", skip(state))]
async fn put_write_filename(
    extract::State(state): extract::State<AppState>,
    extract::Path(filename): extract::Path<String>,
    extract::Query(query): extract::Query<WriteFilenameArgs>,
    request: extract::Request,
) -> Result<(), ApiError> {
    let bucket = state
        .store
        .bucket(&query.bucket_name)
        .await
        .map_err(|e| ApiError::from_bucket_open_error(&query.bucket_name, e))?;

    match bucket.create_file(&filename).await {
        // File doesn't exist, so we upload till we run out of data
        Ok(mut file) => {
            let mut data_stream = request.into_data_stream();
            while let Some(data) = data_stream.try_next().await.map_err(ApiError::internal)? {
                file.append(&data).await.map_err(ApiError::internal)?;
            }
            file.commit().await.map_err(ApiError::internal)?;

            Ok(())
        }
        // File exists, but it might be a no-op re-send of a request
        Err(FileCreateError::FileExists) => {
            // Anything going wrong accesing the file should have been
            // diagnosed elsewhere above.
            let existing_file = bucket.file(&filename).await.map_err(ApiError::internal)?;

            match check_range_matches(request.into_data_stream(), 0, existing_file)
                .await
                .map_err(ApiError::internal)?
            {
                RangeMatch::Matches => Ok(()),
                _ => Err(ApiError::FileExistsWithConflictingContent),
            }
        }
        Err(err) => Err(ApiError::internal(err)),
    }
}

// Note [Streaming request bodies atomicity]:
//
// Technically the request could partially finish before dying, and either we have to
// fully buffer the request body in memory (maybe fine) or write it to the
// final file as we go (which means that we don't have atomicity in the
// case of surprise-disconnects). We can also ignore it for now since we don't
// care *that* much about durability since these are just build log files.
//
// A design change that would fix this is to use copy_file_range to
// leverage modern CoW filesystems such that appending to a file is done by
// appending to a buffer then renaming over the original file. However,
// this breaks inode based locking schemes and would require locking on the
// basis of canonicalized filename. In short: also eww edge cases!
//
// Alternatively we could use directories for each file and write it as parts;
// that seems simply worse however. Overall this is a reminder that all of this
// stuff is really hard.

/// POST `/v0/append/:filename?bucketName=:bucketName&writeOffset=:writeOffset`
///
/// Streams the request body.
#[tracing::instrument(level = "info", skip(state))]
async fn post_append_filename(
    extract::State(state): extract::State<AppState>,
    extract::Path(filename): extract::Path<String>,
    extract::Query(query): extract::Query<AppendFilenameArgs>,
    request: extract::Request,
) -> Result<(), ApiError> {
    // FIXME(jadel): is it semantically acceptable to stream the request body?
    // See Note [Streaming request bodies atomicity].

    let bucket = state
        .store
        .bucket(&query.bucket_name)
        .await
        .map_err(|e| ApiError::from_bucket_open_error(&query.bucket_name, e))?;

    let mut file = bucket
        .file(&filename)
        .await
        .map_err(|e| ApiError::from_bucket_file_open_error(&filename, e))?;

    // File exists, so we upload till we run out of data
    // First, make sure that we aren't an idempotency request.
    let size = file
        .seek(SeekFrom::End(0))
        .await
        .map_err(ApiError::internal)?;
    if query.write_offset <= size {
        return match check_range_matches(request.into_data_stream(), query.write_offset, file)
            .await
            .map_err(ApiError::internal)?
        {
            RangeMatch::Matches => Ok(()),
            RangeMatch::LengthMismatch => Err(ApiError::FileExistsWithConflictingContent),
            RangeMatch::DataMismatch => Err(ApiError::FileExistsWithConflictingContent),
        };
    }

    // This is not an idempotency request, so we can safely append.

    let mut data_stream = request.into_data_stream();
    while let Some(data) = data_stream.try_next().await.map_err(ApiError::internal)? {
        file.append(&data).await.map_err(ApiError::internal)?;
    }
    file.commit().await.map_err(ApiError::internal)?;

    Ok(())
}

#[tracing::instrument(level = "info", skip(state))]
async fn get_buck2_log(
    state: extract::State<AppState>,
    extract::Path(filename): extract::Path<String>,
) -> Result<Response<Body>, ApiError> {
    explore::get_explore(
        state,
        extract::Path(("buck2_logs".to_owned(), format!("flat/{filename}.pb.zst"))),
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use axum::BoxError;

    use super::*;

    #[tokio::test]
    async fn test_check_range_matches() -> Result<(), BoxError> {
        let body_reader = vec![
            Ok(Bytes::from_static(b"kitty meow")),
            Ok(Bytes::from_static(b"creature")),
        ];

        let file = b"kitty meowcreature";

        let result =
            check_range_matches(tokio_stream::iter(body_reader), 0, Cursor::new(file)).await?;
        assert_eq!(result, RangeMatch::Matches);

        Ok(())
    }
}
