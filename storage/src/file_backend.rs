//! A file storage backend based on the filesystem.
//!
//! File metadata is stored in xattrs on the files themselves.
//!
//! In its current design it doesn't provide atomicity for multiple append
//! calls (e.g. as might be required for streaming HTTP bodies). In the future
//! this might want to be changed, see the README's unanswered questions
//! section.
//!
//! There is race safety for accessing individual files.
use std::{
    collections::{HashMap, hash_map::Entry},
    io::{ErrorKind, SeekFrom},
    os::fd::{AsFd, AsRawFd, FromRawFd, IntoRawFd, OwnedFd},
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use pin_project::pin_project;
use rustix::{
    fs::{Mode, OFlags},
    io::Errno,
};
use tokio::io::{AsyncRead, AsyncSeek, AsyncWriteExt};
use xattr::FileExt;

use crate::{
    BoxError, Bucket, FileCreateError, FileHandleOps, FileMetadata, FileOpenError, StorageBackend,
};

/// File storage backend
pub struct FileBackend {
    root_dir: PathBuf,
    // Inner arc is required to be able to pull the inner buckets out of the
    // mutex for read ops.
    buckets: tokio::sync::Mutex<HashMap<String, Arc<FileBucket>>>,
}

impl FileBackend {
    /// FIXME(jadel): maybe this should use a dirfd as well to ensure that the
    /// buckets actually exist, but getting dir listing wired into that might
    /// be unfun.
    pub fn new(root_dir: PathBuf) -> FileBackend {
        FileBackend {
            root_dir,
            buckets: Default::default(),
        }
    }
}

/// An inode number: identifier for physical file on disk.
#[expect(dead_code)]
#[derive(Clone, Copy, Debug)]
struct Inode(pub u64);

/// Pool of locks for each inode we deal with (deals with path normalization
/// (two files that are the same that have a different path) and prevents it
/// from causing concurrency bugs)
///
/// FIXME(jadel): as you might see, this is one mutex. It should be a structure
/// that locks each inode (~= u64) with zero memory use per u64 not being
/// locked.
///
/// FIXME(jadel): some way of forcing the file handle itself into the mutex?
#[derive(Default, Debug)]
struct LockPool(tokio::sync::Mutex<()>);

impl LockPool {
    /// Takes the lock on the given inode
    #[tracing::instrument(level = "debug")]
    async fn lock(&self, _inode: Inode) -> tokio::sync::MutexGuard<'_, ()> {
        self.0.lock().await
    }
}

/// Bucket on the filesystem.
///
/// This is basically just a directory. We use dirfds here to involve fewer
/// directory operations as well as to be pretty confident that there are no
/// escapes from the directory (openat with paths known to not contain any ..s).
///
/// Only one of these objects is allowed to exist for each directory.
#[derive(Debug)]
pub struct FileBucket {
    dirfd: OwnedFd,
    lock_pool: LockPool,
}

impl FileBucket {
    fn open(path: &Path) -> std::io::Result<FileBucket> {
        let dirfd = rustix::fs::open(
            path,
            OFlags::DIRECTORY | OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;

        Ok(FileBucket {
            dirfd,
            lock_pool: LockPool::default(),
        })
    }
}

/// File for xattrs use only. This *shares an fd with a [`tokio::fs::File`]*.
/// Do not do any operations that are visible to `file` on this!
#[derive(Debug)]
struct XattrsFd(OwnedFd);

impl XattrsFd {
    /// Duplicates a file descriptor for xattrs use.
    ///
    /// Requires that it's a file descriptor that actually supports xattrs,
    /// i.e. an actual file; no checks are done to ensure that.
    fn from_dup_fd(fd: impl AsFd) -> Result<Self, Errno> {
        let file = rustix::io::fcntl_dupfd_cloexec(fd, 0)?;
        Ok(XattrsFd(file))
    }
}

impl AsRawFd for XattrsFd {
    fn as_raw_fd(&self) -> std::os::unix::prelude::RawFd {
        self.0.as_raw_fd()
    }
}

impl xattr::FileExt for XattrsFd {}

#[derive(Debug)]
#[pin_project]
pub struct FileHandle<'a> {
    #[pin]
    file: tokio::fs::File,
    file_for_xattrs: Arc<XattrsFd>,
    _lock_handle: tokio::sync::MutexGuard<'a, ()>,
}

/// See xattr(7): we are in the user namespace and we would like to not
/// conflict with other implementations so we use a reverse domain name.
///
/// <https://man7.org/linux/man-pages/man7/xattr.7.html>
fn xattr_name(attr: &str) -> String {
    format!("user.com.mercury.locally-euclidean.{}", attr)
}

/// Error while getting/setting file attributes
#[derive(Debug, thiserror::Error)]
enum FileAttrError {
    #[error("Failed to call get_xattr: {0}")]
    GetXattrFailed(std::io::Error),
    #[error("Corrupt attribute data: could not decode UTF8: {0}")]
    InvalidUtf8(std::string::FromUtf8Error),
    #[error("Failed to call set_xattr: {0}")]
    SetXattrFailed(std::io::Error),
    #[error("Tokio task join failed (BUG): {0}")]
    JoinFailedBug(tokio::task::JoinError),
}

#[async_trait]
impl FileHandleOps for FileHandle<'_> {
    #[tracing::instrument(level = "debug")]
    async fn append(&mut self, data: &[u8]) -> Result<(), BoxError> {
        // XXX: this needs to be called inside a tokio::spawn task so that we
        // don't have partial writes due to cancellation safety issues.
        //
        // This appends to the end of the file since the file is open in append
        // mode.
        //
        // N.B. This also seeks to the end due to POSIX semantics.
        self.file.write_all(data).await?;
        Ok(())
    }

    #[tracing::instrument(level = "debug")]
    async fn get_attr(&mut self, attr: &str) -> Result<Option<String>, BoxError> {
        let file_for_xattrs = self.file_for_xattrs.clone();
        let attr_name = xattr_name(attr);

        match tokio::task::spawn_blocking(move || file_for_xattrs.get_xattr(attr_name))
            .await
            .map_err(FileAttrError::JoinFailedBug)?
            .map_err(FileAttrError::GetXattrFailed)?
        {
            Some(v) => Ok(Some(
                String::from_utf8(v).map_err(FileAttrError::InvalidUtf8)?,
            )),
            None => Ok(None),
        }
    }

    #[tracing::instrument(level = "debug")]
    async fn set_attr(&mut self, attr: &str, value: &str) -> Result<(), BoxError> {
        let file_for_attrs = self.file_for_xattrs.clone();
        let attr_name = xattr_name(attr);
        let value = value.to_owned();

        Ok(tokio::task::spawn_blocking(move || {
            file_for_attrs.set_xattr(attr_name, value.as_bytes())
        })
        .await
        .map_err(FileAttrError::JoinFailedBug)?
        .map_err(FileAttrError::SetXattrFailed)?)
    }

    async fn metadata(&mut self) -> Result<FileMetadata, BoxError> {
        let metadata = self.file.metadata().await?;
        Ok(FileMetadata {
            size: metadata.len(),
        })
    }
}

impl AsyncSeek for FileHandle<'_> {
    fn start_seek(self: std::pin::Pin<&mut Self>, position: SeekFrom) -> std::io::Result<()> {
        AsyncSeek::start_seek(self.project().file, position)
    }

    fn poll_complete(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<u64>> {
        AsyncSeek::poll_complete(self.project().file, cx)
    }
}

impl AsyncRead for FileHandle<'_> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        AsyncRead::poll_read(self.project().file, cx, buf)
    }
}

#[derive(Debug, thiserror::Error)]
enum FilePathError {
    #[error(".. and . components are not allowed in filenames")]
    DotDot,
    #[error("Absolute paths are not allowed")]
    Absolute,
    #[error("Invalid character in filename")]
    InvalidChar,
}

impl FileHandle<'_> {
    /// "Sanitizes" a file path, mostly just by removing blatantly illegal
    /// contents from it. Other than that it's Unix, whatever, it can deal with
    /// pretty nonsense file names that are valid UTF-8.
    fn make_acceptable_filepath(name: &Utf8Path) -> Result<Utf8PathBuf, FilePathError> {
        if name.as_str().contains('\0') {
            return Err(FilePathError::InvalidChar);
        }

        name.components()
            .map(|part| match part {
                Utf8Component::Prefix(_) => unreachable!("Does not occur on Unix"),
                Utf8Component::RootDir => Err(FilePathError::Absolute),
                Utf8Component::CurDir => Err(FilePathError::DotDot),
                Utf8Component::ParentDir => Err(FilePathError::DotDot),
                Utf8Component::Normal(part) => Ok(part),
            })
            .collect::<Result<Utf8PathBuf, _>>()
    }
}

#[derive(Debug, thiserror::Error)]
enum FileBucketError {
    #[error("error calling openat: {0}")]
    OpenatFailed(Errno),
    #[error("error calling fcntl_dupfd_cloexec: {0}")]
    DupFailed(Errno),
    #[error("error calling fstat: {0}")]
    FstatFailed(Errno),
    #[error("invalid file name {0:?}: {1}")]
    BadFileName(String, FilePathError),
}

impl From<FileBucketError> for FileOpenError {
    fn from(value: FileBucketError) -> Self {
        Self::OtherError(value.into())
    }
}

impl From<FileBucketError> for FileCreateError {
    fn from(value: FileBucketError) -> Self {
        Self::OtherError(value.into())
    }
}

#[async_trait]
impl Bucket for FileBucket {
    type FileHandle<'a> = FileHandle<'a>;

    // We implement the file opening as openat so that we don't have to do path
    // joins, mostly. This does mean it is less rusty though...
    //
    // FIXME(jadel): technically I guess these block on an async thread? The
    // whole idea of open and blocking is kind of weird overall... It's not
    // intentionally blocking in the kernel (though IO does take time), but
    // tokio punts it to spawn_blocking anyway.

    #[tracing::instrument(level = "debug")]
    #[must_use]
    async fn file(&self, file_name: &str) -> Result<Self::FileHandle<'_>, FileOpenError> {
        let path = FileHandle::make_acceptable_filepath(Utf8Path::new(file_name))
            .map_err(|e| FileBucketError::BadFileName(file_name.to_owned(), e))?;

        let result = rustix::fs::openat(
            &self.dirfd,
            path.into_std_path_buf(),
            OFlags::RDWR | OFlags::APPEND | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        );

        let handle = match result {
            Ok(handle) => handle,
            Err(Errno::NOENT) => Err(FileOpenError::DoesNotExist)?,
            Err(err) => Err(FileBucketError::OpenatFailed(err))?,
        };

        let stat = rustix::fs::fstat(&handle).map_err(FileBucketError::FstatFailed)?;
        let lock = self.lock_pool.lock(Inode(stat.st_ino)).await;
        let file_for_xattrs =
            Arc::new(XattrsFd::from_dup_fd(&handle).map_err(FileBucketError::DupFailed)?);

        Ok(Self::FileHandle {
            // SAFETY: this is an owned open fd
            file: unsafe { tokio::fs::File::from_raw_fd(handle.into_raw_fd()) },
            file_for_xattrs,
            _lock_handle: lock,
        })
    }

    #[tracing::instrument(level = "debug")]
    #[must_use]
    async fn create_file(&self, file_name: &str) -> Result<Self::FileHandle<'_>, FileCreateError> {
        let path = FileHandle::make_acceptable_filepath(Utf8Path::new(file_name))
            .map_err(|e| FileBucketError::BadFileName(file_name.to_owned(), e))?;

        let result = rustix::fs::openat(
            &self.dirfd,
            path.into_std_path_buf(),
            // CREATE | EXCL: create the file if it doesn't exist, fail if it does
            OFlags::RDWR
                | OFlags::APPEND
                | OFlags::CREATE
                | OFlags::EXCL
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::ROTH,
        );

        let handle = match result {
            Ok(handle) => handle,
            Err(Errno::EXIST) => Err(FileCreateError::FileExists)?,
            Err(err) => Err(FileBucketError::OpenatFailed(err))?,
        };

        let stat = rustix::fs::fstat(&handle).map_err(FileBucketError::FstatFailed)?;
        let lock = self.lock_pool.lock(Inode(stat.st_ino)).await;
        let file_for_xattrs =
            Arc::new(XattrsFd::from_dup_fd(&handle).map_err(FileBucketError::DupFailed)?);

        Ok(Self::FileHandle {
            // SAFETY: this is an owned open fd
            file: unsafe { tokio::fs::File::from_raw_fd(handle.into_raw_fd()) },
            file_for_xattrs,
            _lock_handle: lock,
        })
    }
}

#[async_trait]
impl StorageBackend for FileBackend {
    type Bucket<'a> = Arc<FileBucket>;

    #[tracing::instrument(level = "debug", skip(self))]
    #[must_use]
    async fn bucket(&self, name: &str) -> Result<Self::Bucket<'_>, FileOpenError> {
        // These would constitute path traversal bugs when combined with
        // PathBuf::join
        if name.contains(['/', '\0']) || name == "." || name == ".." {
            return Err(FileOpenError::InvalidName);
        }

        let mut buckets = self.buckets.lock().await;
        match buckets.entry(name.to_owned()) {
            Entry::Occupied(entry) => Ok(entry.get().clone()),
            Entry::Vacant(entry) => {
                let path = self.root_dir.join(name);

                let bucket = FileBucket::open(&path).map_err(|e| match e.kind() {
                    ErrorKind::NotFound => FileOpenError::DoesNotExist,
                    _ => FileOpenError::OtherError(e.into()),
                })?;

                let bucket = entry.insert(Arc::new(bucket));
                Ok((*bucket).clone())
            }
        }
    }

    #[tracing::instrument(level = "debug", skip(self))]
    #[must_use]
    async fn list_buckets(&self) -> Result<Vec<String>, BoxError> {
        let mut iter = tokio::fs::read_dir(&self.root_dir).await?;
        let mut names = Vec::new();

        while let Some(entry) = iter.next_entry().await? {
            let type_ = entry.file_type().await?;
            if type_.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    names.push(name.to_owned());
                }
            }
        }
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use std::{ops::Deref, time::Duration};

    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    use super::*;

    struct TempFileBackend {
        pub temp_dir: TempDir,
        pub file_backend: FileBackend,
    }

    impl TempFileBackend {
        fn new(name: &str) -> Result<TempFileBackend, BoxError> {
            let temp_dir = TempDir::with_suffix(name)?;
            let backend = FileBackend::new(temp_dir.path().to_owned());
            Ok(TempFileBackend {
                temp_dir,
                file_backend: backend,
            })
        }

        async fn new_with_bucket(name: &str) -> Result<TempFileBackend, BoxError> {
            let result = Self::new(name)?;
            tokio::fs::create_dir(result.temp_dir.path().join("bucket")).await?;
            Ok(result)
        }
    }

    impl Deref for TempFileBackend {
        type Target = FileBackend;

        fn deref(&self) -> &Self::Target {
            &self.file_backend
        }
    }

    #[tokio::test]
    async fn test_lists_buckets() -> Result<(), BoxError> {
        let backend = TempFileBackend::new("test_lists_buckets")?;

        assert!(backend.list_buckets().await?.is_empty());

        tokio::fs::create_dir(backend.temp_dir.path().join("bukkit")).await?;

        assert_eq!(backend.list_buckets().await?, vec!["bukkit"]);
        Ok(())
    }

    #[tokio::test]
    async fn test_bucket_create_file_once() -> Result<(), BoxError> {
        let backend = TempFileBackend::new_with_bucket("test_bucket_create_file_once").await?;

        assert!(matches!(
            backend.bucket("nonexistent").await,
            Err(FileOpenError::DoesNotExist)
        ));
        let bucket = backend.bucket("bucket").await?;

        assert!(matches!(
            bucket.file("meow").await,
            Err(FileOpenError::DoesNotExist)
        ));

        {
            let mut handle = bucket.create_file("meow").await?;
            handle.append(b"meow").await?;
            handle.append(b"awoo").await?;
            handle.seek(SeekFrom::Start(2)).await?;

            let mut buf = vec![0; 4];
            handle.read_exact(&mut buf).await?;
            assert_eq!(buf, b"owaw");
        }

        assert!(matches!(
            bucket.create_file("meow").await,
            Err(FileCreateError::FileExists)
        ));

        {
            let mut handle = bucket.file("meow").await?;
            let mut content = String::new();
            handle.read_to_string(&mut content).await?;
            assert_eq!(content, "meowawoo");
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_open_same_file_concurrency() -> Result<(), BoxError> {
        let backend = TempFileBackend::new_with_bucket("test_open_same_file_concurrency").await?;

        let bucket = backend.bucket("bucket").await?;
        let handle1 = bucket.create_file("meow").await?;
        let handle2_fut = bucket.file("meow");
        let timeout = tokio::time::timeout(Duration::from_millis(10), handle2_fut).await;
        // This must time out since we don't allow concurrency on individual
        // files
        assert!(timeout.is_err());

        let handle2_fut = bucket.file("meow");
        drop(handle1);
        let _ = handle2_fut.await.unwrap();
        Ok(())
    }

    #[tokio::test]
    async fn test_attrs() -> Result<(), BoxError> {
        let backend = TempFileBackend::new_with_bucket("test_open_same_file_concurrency").await?;
        let bucket = backend.bucket("bucket").await?;
        let mut handle1 = bucket.create_file("meow").await?;

        handle1.set_attr("meow", "kbity").await?;
        assert_eq!(handle1.get_attr("meow").await?, Some("kbity".to_owned()));

        Ok(())
    }

    #[tokio::test]
    async fn test_metadata() -> Result<(), BoxError> {
        let backend = TempFileBackend::new_with_bucket("test_open_same_file_concurrency").await?;
        let bucket = backend.bucket("bucket").await?;
        let mut handle1 = bucket.create_file("meow").await?;

        let FileMetadata { size } = handle1.metadata().await?;
        assert_eq!(size, 0);

        handle1.append(b"meow").await?;

        let FileMetadata { size } = handle1.metadata().await?;
        assert_eq!(size, 4);
        Ok(())
    }
}
