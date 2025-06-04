//! Blob library for postgres using [Large
//! Objects](https://www.postgresql.org/docs/17/largeobjects.html).
use std::{
    io::SeekFrom,
    ops::{Deref, DerefMut},
    pin::Pin,
    sync::Arc,
    task::{Poll, ready},
};

use futures_core::future::BoxFuture;
use nix::unistd::Whence;
use sqlx::{Execute, PgPool, PgTransaction, postgres::types::Oid};
use tokio::sync::Mutex as TokioMutex;
use tokio::{
    io::{AsyncRead, AsyncSeek, AsyncWrite},
    sync::OwnedMutexGuard,
};

/// File handle to a file in the Postgres store.
///
/// This is a *handle*: it's using the Postgres [Large
/// Objects](https://www.postgresql.org/docs/17/largeobjects.html) feature to
/// store the actual data.
struct BlobHandleInner {
    /// We have to hold a transaction open as the handles are only valid within
    /// one transaction.
    transaction: sqlx::PgTransaction<'static>,
    fd_num: i32,
}

/// The state that a tokio-compatible file handle is in.
///
/// It can only be in one state at once, and cancellation can mean that
/// [`AsyncRead`] could be interrupted and then the handle is used to do an
/// [`AsyncSeek`] or whatever. We don't support that and simply panic about it.
#[non_exhaustive]
enum FileHandleState {
    Reading(BoxFuture<'static, Result<Vec<u8>, FileHandleError>>),
    Seeking(BoxFuture<'static, Result<i64, FileHandleError>>),
    Writing(BoxFuture<'static, Result<i32, FileHandleError>>),
    ShuttingDown(BoxFuture<'static, Result<(), FileHandleError>>),
}

/// File handle to a file in the Postgres store. This provides
/// [`AsyncRead`]/[`AsyncWrite`]/[`AsyncSeek`].
///
/// This is a *handle*: it's using the Postgres [Large
/// Objects](https://www.postgresql.org/docs/17/largeobjects.html) feature to
/// store the actual data.
pub struct BlobHandle {
    /// Option in here represents the file handle being closed without the type
    /// system being able to be told of it.
    inner: Arc<TokioMutex<Option<BlobHandleInner>>>,
    /// Whether some operation is in progress, basically.
    state: Option<FileHandleState>,
    /// Okay so this is bizarre but: AsyncSeek expects to be able to get a
    /// stale result from calling poll_complete() multiple times, but at least
    /// [`tokio::fs::File`] doesn't actually track this for any operations
    /// except seek (so it *will* become stale).
    position: u64,
}

const MIB: usize = 2 * 1024 * 1024;

/// A wrapper around a DB transaction extracted from a blob handle. This
/// prevents you doing anything else with the blob handle while it's held.
///
/// The alternative to this is that we somehow force externally providing a
/// transaction which seems *also* bad due to lifetimes.
pub struct TransactionWrapper(OwnedMutexGuard<Option<BlobHandleInner>>);

impl Deref for TransactionWrapper {
    type Target = PgTransaction<'static>;

    fn deref(&self) -> &Self::Target {
        let Some(inner) = &*self.0 else {
            panic!("tried to access a closed blob handle")
        };
        &inner.transaction
    }
}

impl DerefMut for TransactionWrapper {
    fn deref_mut(&mut self) -> &mut Self::Target {
        let Some(inner) = &mut *self.0 else {
            panic!("tried to access a closed blob handle")
        };
        &mut inner.transaction
    }
}

impl BlobHandle {
    /// Opens a file handle by OID.
    pub async fn open(pool: PgPool, oid: Oid, mode: OpenMode) -> Result<Self, FileHandleError> {
        let inner = BlobHandleInner::open(pool, oid, mode).await?;
        Ok(BlobHandle {
            inner: Arc::new(TokioMutex::new(Some(inner))),
            state: None,
            position: 0,
        })
    }

    pub async fn transaction(&mut self) -> TransactionWrapper {
        TransactionWrapper(self.inner.clone().lock_owned().await)
    }
}

impl AsyncRead for BlobHandle {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        loop {
            if let Some(ref mut op) = self.state {
                match op {
                    FileHandleState::Reading(future) => {
                        let result = ready!(future.as_mut().poll(cx));
                        self.state = None;

                        break Poll::Ready(match result {
                            Ok(data) => {
                                // Annoyingly AsyncRead means that we may have to
                                // deal with some goofball giving us a smaller
                                // buffer than we started out with and then we
                                // probably *should* keep that data around.
                                // Doing that seems like it might be hard to write
                                // correctly.
                                //
                                // Fortunately, we can just panic about it instead.
                                buf.put_slice(&data);
                                Ok(())
                            }
                            Err(e) => Err(std::io::Error::other(e)),
                        });
                    }
                    _ => panic!(
                        "postgres file handle future was cancelled in the middle of another operation"
                    ),
                }
            } else {
                let inner = self.inner.clone();
                let remaining = u32::try_from(buf.remaining().min(8 * MIB)).unwrap();

                self.state = Some(FileHandleState::Reading(Box::pin(async move {
                    let mut inner = inner.lock_owned().await;
                    inner
                        .as_mut()
                        .ok_or(FileHandleError::IsShutdown)?
                        .read(remaining)
                        .await
                })));
                // Back around the loop with us in case this mysteriously
                // returns immediately.
            }
        }
    }
}

impl AsyncSeek for BlobHandle {
    fn start_seek(self: Pin<&mut Self>, position: SeekFrom) -> std::io::Result<()> {
        let this = self.get_mut();
        let inner = this.inner.clone();
        this.state = Some(FileHandleState::Seeking(Box::pin(async move {
            let mut inner = inner.lock_owned().await;
            inner
                .as_mut()
                .ok_or(FileHandleError::IsShutdown)?
                .lseek64(position)
                .await
        })));
        Ok(())
    }

    fn poll_complete(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<u64>> {
        if let Some(ref mut state) = self.state {
            match state {
                FileHandleState::Seeking(future) => {
                    let result = ready!(future.as_mut().poll(cx));
                    self.state = None;

                    let result = result
                        .map(|offset| {
                            offset
                                .try_into()
                                .expect("postgres seek gives negative value")
                        })
                        .map_err(std::io::Error::other)?;
                    self.position = result;

                    Poll::Ready(Ok(result))
                }
                _ => panic!(
                    "postgres file handle future was cancelled in the middle of another operation"
                ),
            }
        } else {
            Poll::Ready(Ok(self.position))
        }
    }
}

impl AsyncWrite for BlobHandle {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        loop {
            if let Some(ref mut state) = self.state {
                match state {
                    FileHandleState::Writing(future) => {
                        let result = ready!(future.as_mut().poll(cx));
                        self.state = None;

                        break Poll::Ready(
                            result
                                .map(|written| written.try_into().expect("wrote negative amount?"))
                                .map_err(std::io::Error::other),
                        );
                    }
                    _ => panic!(
                        "postgres file handle future was cancelled in the middle of another operation"
                    ),
                }
            } else {
                let inner = self.inner.clone();
                let data = buf.to_owned();

                self.state = Some(FileHandleState::Writing(Box::pin(async move {
                    let mut inner = inner.lock_owned().await;
                    inner
                        .as_mut()
                        .ok_or(FileHandleError::IsShutdown)?
                        .write(&data)
                        .await
                })))
            }
        }
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        // No such thing as flush here, since our IO is directly into an ACID
        // DBMS.
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        loop {
            if let Some(ref mut state) = self.state {
                match state {
                    FileHandleState::ShuttingDown(future) => {
                        let result = ready!(future.as_mut().poll(cx));
                        self.state = None;

                        break Poll::Ready(result.map_err(std::io::Error::other));
                    }
                    _ => panic!(
                        "postgres file handle future was cancelled in the middle of another operation"
                    ),
                }
            } else {
                let inner = self.inner.clone();

                self.state = Some(FileHandleState::ShuttingDown(Box::pin(async move {
                    let mut inner = inner.lock_owned().await;
                    inner
                        .take()
                        .ok_or(FileHandleError::IsShutdown)?
                        .close()
                        .await
                })))
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FileHandleError {
    #[error("Failed file handle query {0:?}: {1}")]
    QueryFailed(String, sqlx::Error),
    #[error("Failed to acquire transaction")]
    TransactionBeginFailed(&'static str, sqlx::Error),
    #[error("Failed to acquire transaction")]
    TransactionCommitFailed(&'static str, sqlx::Error),
    #[error("Empty result from lo_lseek64")]
    EmptyLseek64,
    #[error("Empty result from lo_open")]
    EmptyOpen,
    #[error("Empty result from loread")]
    EmptyRead,
    #[error("Empty result from lowrite")]
    EmptyWrite,
    #[error("Empty result from lo_close")]
    EmptyClose,
    #[error("Calling methods on a shutdown handle")]
    IsShutdown,
}

/// Mode to open a file with, corresponding with `INV_READ` and `INV_WRITE` in
/// Postgres land (`libpq-fs.h`).
#[derive(Debug)]
#[repr(i32)]
pub enum OpenMode {
    /// `INV_READ`
    Read = 0x0002_0000,
    /// `INV_READ | INV_WRITE`
    ReadWrite = 0x0002_0000 | 0x0004_0000,
}

impl BlobHandleInner {
    // Thin wrappers around the postgres primitive functions

    /// Opens the given oid as a file.
    async fn open(pool: sqlx::PgPool, oid: Oid, mode: OpenMode) -> Result<Self, FileHandleError> {
        let query = sqlx::query!("select lo_open($1, $2)", oid, mode as i32);
        let mut transaction = pool
            .begin()
            .await
            .map_err(|e| FileHandleError::TransactionBeginFailed("lo_open", e))?;

        let sql = query.sql();
        let handle = query
            .fetch_one(&mut *transaction)
            .await
            .map_err(|e| FileHandleError::QueryFailed(sql.to_owned(), e))?;

        Ok(Self {
            transaction,
            fd_num: handle.lo_open.ok_or(FileHandleError::EmptyOpen)?,
        })
    }

    /// Seeks to the given position in a file.
    async fn lseek64(&mut self, seek: SeekFrom) -> Result<i64, FileHandleError> {
        let (n, whence) = match seek {
            SeekFrom::Start(n) => (n as i64, Whence::SeekSet),
            SeekFrom::Current(n) => (n, Whence::SeekCur),
            SeekFrom::End(n) => (n, Whence::SeekEnd),
        };
        let whence = whence as i32;

        let query = sqlx::query!("select lo_lseek64($1, $2, $3)", self.fd_num, n, whence);
        let sql = query.sql();
        let result = query
            .fetch_one(&mut *self.transaction)
            .await
            .map_err(|e| FileHandleError::QueryFailed(sql.to_owned(), e))?;

        result.lo_lseek64.ok_or(FileHandleError::EmptyLseek64)
    }

    /// Reads bytes from the file at the current position.
    async fn read(&mut self, count: u32) -> Result<Vec<u8>, FileHandleError> {
        let query = sqlx::query!("select loread($1, $2)", self.fd_num, count as i32);
        let sql = query.sql();
        let result = query
            .fetch_one(&mut *self.transaction)
            .await
            .map_err(|e| FileHandleError::QueryFailed(sql.to_owned(), e))?;

        result.loread.ok_or(FileHandleError::EmptyRead)
    }

    /// Writes bytes to the file at the current position
    async fn write(&mut self, data: &[u8]) -> Result<i32, FileHandleError> {
        let query = sqlx::query!("select lowrite($1, $2)", self.fd_num, data);
        let sql = query.sql();
        let result = query
            .fetch_one(&mut *self.transaction)
            .await
            .map_err(|e| FileHandleError::QueryFailed(sql.to_owned(), e))?;

        result.lowrite.ok_or(FileHandleError::EmptyWrite)
    }

    /// File handles must be closed or else they *will* just get rolled back!
    /// This includes dropping them. Async drop isn't real and can't hurt you.
    async fn close(mut self) -> Result<(), FileHandleError> {
        let query = sqlx::query!("select lo_close($1)", self.fd_num);
        let sql = query.sql();
        let result = query
            .fetch_one(&mut *self.transaction)
            .await
            .map_err(|e| FileHandleError::QueryFailed(sql.to_owned(), e))?;

        let _ = result.lo_close.ok_or(FileHandleError::EmptyClose)?;
        self.transaction
            .commit()
            .await
            .map_err(|e| FileHandleError::TransactionCommitFailed("lo_close", e))?;
        Ok(())
    }
}
