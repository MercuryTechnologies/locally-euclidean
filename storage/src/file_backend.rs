use std::{
    collections::{HashMap, hash_map::Entry},
    io::{ErrorKind, SeekFrom},
    os::fd::{FromRawFd, IntoRawFd, OwnedFd},
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

use crate::{BoxError, Bucket, FileCreateError, FileHandleOps, FileOpenError, StorageBackend};

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

#[pin_project]
pub struct FileHandle<'a> {
    #[pin]
    file: tokio::fs::File,
    _lock_handle: tokio::sync::MutexGuard<'a, ()>,
}

#[async_trait]
impl FileHandleOps for FileHandle<'_> {
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

    #[must_use]
    async fn file(&self, file_name: &str) -> Result<Self::FileHandle<'_>, FileOpenError> {
        let path = FileHandle::make_acceptable_filepath(Utf8Path::new(file_name))
            .map_err(|e| FileOpenError::OtherError(e.into()))?;

        let result = rustix::fs::openat(
            &self.dirfd,
            path.into_std_path_buf(),
            OFlags::RDWR | OFlags::APPEND | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        );

        let handle = match result {
            Ok(handle) => handle,
            Err(Errno::NOENT) => Err(FileOpenError::DoesNotExist)?,
            Err(err) => Err(FileOpenError::OtherError(err.into()))?,
        };

        let stat = rustix::fs::fstat(&handle).map_err(|e| FileOpenError::OtherError(e.into()))?;
        let lock = self.lock_pool.lock(Inode(stat.st_ino)).await;
        Ok(Self::FileHandle {
            // SAFETY: this is an owned open fd
            file: unsafe { tokio::fs::File::from_raw_fd(handle.into_raw_fd()) },
            _lock_handle: lock,
        })
    }

    #[must_use]
    async fn create_file(&self, file_name: &str) -> Result<Self::FileHandle<'_>, FileCreateError> {
        let path = FileHandle::make_acceptable_filepath(Utf8Path::new(file_name))
            .map_err(|e| FileCreateError::OtherError(e.into()))?;

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
            Err(err) => Err(FileCreateError::OtherError(err.into()))?,
        };

        let stat = rustix::fs::fstat(&handle).map_err(|e| FileCreateError::OtherError(e.into()))?;
        let lock = self.lock_pool.lock(Inode(stat.st_ino)).await;
        Ok(Self::FileHandle {
            // SAFETY: this is an owned open fd
            file: unsafe { tokio::fs::File::from_raw_fd(handle.into_raw_fd()) },
            _lock_handle: lock,
        })
    }
}

#[async_trait]
impl StorageBackend for FileBackend {
    type Bucket<'a> = Arc<FileBucket>;

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
}
