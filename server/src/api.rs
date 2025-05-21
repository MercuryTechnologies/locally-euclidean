//! The Manifold API implementation.

use std::{
    io::{ErrorKind, SeekFrom},
    sync::Arc,
};

use axum::{
    body::Bytes,
    extract,
    http::StatusCode,
    routing::{post, put},
};
use http_body_util::BodyExt;
use storage::{Bucket, FileCreateError, FileHandleOps, FileOpenError, StorageBackend};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt},
    sync::Mutex as TokioMutex,
};
use tokio_stream::{Stream, StreamExt};

use crate::{AppState, errors::ServiceError, service_error};

pub fn make_router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/write/{*filename}", put(put_write_filename))
        .route("/append/{*filename}", post(post_append_filename))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriteFilenameArgs {
    bucket_name: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppendFilenameArgs {
    bucket_name: String,
    write_offset: u64,
}

#[derive(thiserror::Error, Debug)]
enum ApiError {
    #[error("Bucket does not exist")]
    BucketDoesNotExist,
    #[error("Invalid bucket name")]
    InvalidBucketName,
    #[error("File does not exist")]
    FileDoesNotExist,
    #[error("Invalid file name")]
    InvalidFileName,
    #[error("File already exists with conflicting content")]
    FileExistsWithConflictingContent,
    #[error("Internal error: {0}")]
    InternalError(axum::BoxError),
}

impl ServiceError for ApiError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::BucketDoesNotExist => StatusCode::NOT_FOUND,
            Self::InvalidBucketName => StatusCode::BAD_REQUEST,
            Self::FileDoesNotExist => StatusCode::NOT_FOUND,
            Self::InvalidFileName => StatusCode::BAD_REQUEST,
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
    fn internal(err: impl Into<axum::BoxError>) -> Self {
        Self::InternalError(err.into())
    }

    // This is not a From impl since FileOpenError can occur for more than just
    // opening a bucket.

    fn from_bucket_open_error(err: FileOpenError) -> ApiError {
        match err {
            FileOpenError::DoesNotExist => ApiError::BucketDoesNotExist,
            FileOpenError::InvalidName => ApiError::InvalidBucketName,
            e => Self::internal(e),
        }
    }

    fn from_bucket_file_open_error(err: FileOpenError) -> ApiError {
        match err {
            FileOpenError::DoesNotExist => ApiError::FileDoesNotExist,
            FileOpenError::InvalidName => ApiError::InvalidFileName,
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
        .map_err(ApiError::from_bucket_open_error)?;

    match bucket.create_file(&filename).await {
        // File doesn't exist, so we upload till we run out of data
        Ok(file) => {
            // We have to use a bogus mutex since borrowck can't be convinced
            // to rely on tokio::spawn's closure finishing before the next
            // iteration. The mutex is never contended.
            let file = Arc::new(TokioMutex::new(file));

            let mut data_stream = request.into_data_stream();
            while let Some(data) = data_stream.try_next().await.map_err(ApiError::internal)? {
                let file = file.clone();
                // This needs to be uncancellable so that it is somewhat atomic
                // (though see Note [Streaming request bodies atomicity]).
                tokio::spawn(async move {
                    let mut file = file.lock_owned().await;
                    file.append(&data).await
                })
                .await
                .map_err(ApiError::internal)?
                .map_err(ApiError::internal)?;
            }

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
        .map_err(ApiError::from_bucket_open_error)?;

    let mut file = bucket
        .file(&filename)
        .await
        .map_err(ApiError::from_bucket_file_open_error)?;

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

    // We have to use a bogus mutex since borrowck can't be convinced
    // to rely on tokio::spawn's closure finishing before the next
    // iteration. The mutex is never contended.
    let file = Arc::new(TokioMutex::new(file));

    let mut data_stream = request.into_data_stream();
    while let Some(data) = data_stream.try_next().await.map_err(ApiError::internal)? {
        let file = file.clone();
        // This needs to be uncancellable so that it is somewhat atomic
        // (though see Note [Streaming request bodies atomicity]).
        tokio::spawn(async move {
            let mut file = file.lock_owned().await;
            file.append(&data).await
        })
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;
    }

    Ok(())
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
