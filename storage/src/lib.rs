pub mod file_backend;
pub mod postgres;

use async_trait::async_trait;
use chrono::Utc;
use std::ops::Deref;
use tokio::io::{AsyncRead, AsyncSeek};

pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

#[derive(Debug, thiserror::Error)]
pub enum FileOpenError {
    #[error("File does not exist")]
    DoesNotExist,
    #[error("Invalid name")]
    InvalidName,
    #[error("Unknown file open error: {0}")]
    OtherError(BoxError),
}

#[derive(Debug, thiserror::Error)]
pub enum FileCreateError {
    #[error("File exists")]
    FileExists,
    #[error("Invalid name")]
    InvalidName,
    #[error("Unknown file create error: {0}")]
    OtherError(BoxError),
}

/// File metadata.
pub struct FileMetadata {
    /// When the file was last modified, according to the system.
    pub last_modified_at: chrono::DateTime<Utc>,
}

/// Operations allowed on a file handle
#[async_trait]
pub trait FileHandleOps: AsyncRead + AsyncSeek {
    /// Appends some bytes to the file.
    ///
    /// This does not implement the API semantics of checking if the range matches, that's left to a higher level.
    async fn append(&mut self, data: &[u8]) -> Result<(), BoxError>;

    /// Retrieves some common metadata on a file. Currently this is just
    /// modification time.
    async fn metadata(&mut self) -> Result<FileMetadata, BoxError>;

    /// Commits the changes to the file handle to storage.
    async fn commit(self) -> Result<(), BoxError>;
}

/// Storage backend for locally-euclidean.
#[async_trait]
pub trait Bucket {
    /// File handle for this storage backend.
    ///
    /// TODO: is this the correct syntax to require it is self in lifetime?
    type FileHandle: FileHandleOps;

    /// Gets a handle to the given file.
    ///
    /// Currently, at most one handle (which currently means "read/write
    /// handle") may exist to a file at a given time.
    ///
    /// FIXME: liveness semantics?
    async fn file(&self, file_name: &str) -> Result<Self::FileHandle, FileOpenError>;

    /// Creates the given file and gives a handle to it.
    ///
    /// If the file exists, this returns [`FileCreateError::FileExists`], at
    /// which point the higher level should check if the contents match at the
    /// starting range.
    async fn create_file(&self, file_name: &str) -> Result<Self::FileHandle, FileCreateError>;
}

#[async_trait]
impl<T, Inner> Bucket for T
where
    T: Deref<Target = Inner> + Send + Sync + 'static,
    Inner: Bucket + Send + Sync + 'static,
{
    type FileHandle = Inner::FileHandle;

    async fn file(&self, file_name: &str) -> Result<Self::FileHandle, FileOpenError> {
        self.deref().file(file_name).await
    }

    async fn create_file(&self, file_name: &str) -> Result<Self::FileHandle, FileCreateError> {
        self.deref().create_file(file_name).await
    }
}

/// Top level trait for a storage backend.
#[async_trait]
pub trait StorageBackend {
    type Bucket<'a>: Bucket + 'a
    where
        Self: 'a;

    /// Gets a bucket by name. Fails if the bucket doesn't exist.
    async fn bucket(&self, name: &str) -> Result<Self::Bucket<'_>, FileOpenError>;

    /// Lists all the bucket names on the system.
    async fn list_buckets(&self) -> Result<Vec<String>, BoxError>;
}
