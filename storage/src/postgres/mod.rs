//! Postgres storage backend.
//!
//! This is *all* postgres: file data is stored in blobs.
//!
//! FIXME(jadel): this module overall does not handle postgres transactions
//! very thoughtfully, and nor does [`blob`] particularly. Unfortunately
//! lifetimes in async are inviting hell, even though those would definitely
//! encourage more deliberate behaviour.
//!
//! It also needs to handle locking of concurrent access better.

pub mod blob;

use std::io::SeekFrom;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{TimeDelta, Utc};
use pin_project::pin_project;
use sqlx::error::Error as SqlxError;
use sqlx::error::ErrorKind as SqlxErrorKind;
use sqlx::postgres::types::Oid;
use sqlx::{Execute, PgPool, types::Uuid};
use tokio::io::{AsyncRead, AsyncSeek, AsyncSeekExt, AsyncWrite, AsyncWriteExt};

use crate::StorageBackend;
use crate::{BoxError, Bucket, FileCreateError, FileHandleOps, FileMetadata, FileOpenError};
use blob::BlobHandle;

pub use blob::FileHandleError;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../migrations");

/// ID of a db `bucket` object.
#[derive(Clone, Debug, sqlx::Type)]
#[sqlx(transparent)]
struct BucketId(Uuid);

/// ID of a db `file` object.
#[derive(Clone, Debug, sqlx::Type)]
#[sqlx(transparent)]
struct FileId(Uuid);

/// A handle to a *file* object in locally-euclidean.
#[pin_project]
pub struct PostgresFileHandle {
    #[pin]
    blob: BlobHandle,
    id: FileId,
}

impl PostgresFileHandle {
    async fn new(
        pool: PgPool,
        blob_oid: Oid,
        id: FileId,
    ) -> Result<PostgresFileHandle, FileHandleError> {
        let blob = BlobHandle::open(pool, blob_oid, blob::OpenMode::ReadWrite).await?;
        Ok(PostgresFileHandle { blob, id })
    }
}

impl AsyncRead for PostgresFileHandle {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.project().blob.poll_read(cx, buf)
    }
}

impl AsyncSeek for PostgresFileHandle {
    fn start_seek(self: std::pin::Pin<&mut Self>, position: SeekFrom) -> std::io::Result<()> {
        self.project().blob.start_seek(position)
    }
    fn poll_complete(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<u64>> {
        self.project().blob.poll_complete(cx)
    }
}

// We intentionally don't implement AsyncWrite here since we don't *want* to
// offer generalized non-append writes.
fn _assert_impl() {
    assert_impl::assert_impl!(!AsyncWrite: PostgresFileHandle);
}

#[async_trait]
impl FileHandleOps for PostgresFileHandle {
    #[tracing::instrument(level = "debug", skip(self))]
    async fn append(&mut self, data: &[u8]) -> Result<(), BoxError> {
        self.blob.seek(SeekFrom::End(0)).await?;
        self.blob.write_all(data).await?;
        Ok(())
    }

    #[tracing::instrument(level = "debug", skip(self))]
    async fn metadata(&mut self) -> Result<FileMetadata, BoxError> {
        let query = sqlx::query!(
            "select updated_at from files where id = $1",
            &self.id as &FileId
        );
        let sql = query.sql();
        let result = query
            .fetch_one(&mut **self.blob.transaction().await)
            .await
            .map_err(|e| FileHandleError::QueryFailed(sql.to_owned(), e))?;
        Ok(FileMetadata {
            last_modified_at: result.updated_at,
        })
    }

    async fn commit(mut self) -> Result<(), BoxError> {
        self.blob.shutdown().await?;
        Ok(())
    }
}

#[derive(Debug)]
pub struct PostgresBucket {
    pool: PgPool,
    id: BucketId,
    /// Default time after which files in this bucket will be deleted.
    default_ttl: Option<TimeDelta>,
}

/// The miscellaneous errors that happen when dealing with buckets
#[derive(Debug, thiserror::Error)]
pub enum BucketError {
    #[error("Failed bucket query {0:?}: {1}")]
    QueryFailed(String, sqlx::Error),
}

impl PostgresBucket {
    /// Finds a bucket with the given name.
    pub async fn by_name(pool: PgPool, name: &str) -> Result<Option<Self>, BucketError> {
        let query = sqlx::query!(
            r#"select id as "id: BucketId", default_ttl from buckets where name = $1"#,
            name
        );
        let sql = query.sql();

        let result = query.fetch_optional(&pool).await;
        let result = match result {
            Ok(Some(v)) => Ok(v),
            Ok(None) => return Ok(None),
            Err(e) => Err(BucketError::QueryFailed(sql.to_owned(), e)),
        }?;

        Ok(Some(Self {
            pool,
            id: result.id,
            default_ttl: result
                .default_ttl
                // XXX(jadel): this is definitely *technically* wrong for
                // months and days due to uneven month length and DST, but we
                // don't care in this case because it's just for throwing away
                // some build logs
                .map(|ttl| {
                    TimeDelta::days(i64::from(ttl.months) * 30)
                        + TimeDelta::days(ttl.days.into())
                        + TimeDelta::microseconds(ttl.microseconds)
                }),
        }))
    }

    /// Returns a list of all the bucket names on the system.
    pub async fn list(pool: PgPool) -> Result<Vec<String>, BucketError> {
        let query = sqlx::query!("select name from buckets");
        let sql = query.sql();

        let result = query
            .fetch_all(&pool)
            .await
            .map_err(|e| BucketError::QueryFailed(sql.to_owned(), e))?;

        Ok(result.into_iter().map(|record| record.name).collect())
    }

    /// Creates a bucket.
    pub async fn create(
        pool: PgPool,
        name: &str,
        default_ttl: Option<TimeDelta>,
    ) -> Result<Self, FileCreateError> {
        let query = sqlx::query!(
            r#"insert into buckets (name, default_ttl) values ($1, $2) returning id as "id: BucketId""#,
            name,
            default_ttl as Option<TimeDelta>
        );
        let sql = query.sql();

        let result = query.fetch_one(&pool).await;
        let result = match result {
            Ok(v) => Ok(v),
            Err(SqlxError::Database(err)) if err.kind() == SqlxErrorKind::UniqueViolation => {
                Err(FileCreateError::FileExists)
            }
            Err(e) => Err(BucketError::QueryFailed(sql.to_owned(), e).into()),
        }?;

        Ok(Self {
            pool,
            id: result.id,
            default_ttl,
        })
    }
}

#[async_trait]
impl Bucket for PostgresBucket {
    type FileHandle = PostgresFileHandle;

    #[tracing::instrument(level = "debug")]
    async fn file(&self, file_name: &str) -> Result<Self::FileHandle, FileOpenError> {
        let query = sqlx::query!(
            r#"select id as "id: FileId", blob from files where bucket_id = $1 and filename = $2"#,
            &self.id as &BucketId,
            file_name
        );
        let sql = query.sql();

        let result = query.fetch_optional(&self.pool).await;
        let result = match result {
            Ok(Some(v)) => Ok(v),
            Ok(None) => Err(FileOpenError::DoesNotExist),
            Err(e) => Err(BucketError::QueryFailed(sql.to_owned(), e).into()),
        }?;

        Ok(PostgresFileHandle::new(self.pool.clone(), result.blob, result.id).await?)
    }

    #[tracing::instrument(level = "debug")]
    async fn create_file(&self, file_name: &str) -> Result<Self::FileHandle, FileCreateError> {
        let delete_after = self.default_ttl.map(|ttl| Utc::now() + ttl);
        let query = sqlx::query!(
            r#"insert into files (bucket_id, filename, delete_after, blob) values ($1, $2, $3, lo_create(0)) returning id as "id: FileId", blob"#,
            &self.id as &BucketId,
            file_name,
            delete_after
        );
        let sql = query.sql();

        let result = query.fetch_one(&self.pool).await;
        let result = match result {
            Ok(v) => Ok(v),
            Err(SqlxError::Database(err)) if err.kind() == SqlxErrorKind::UniqueViolation => {
                Err(FileCreateError::FileExists)
            }
            Err(e) => Err(BucketError::QueryFailed(sql.to_owned(), e).into()),
        }?;

        Ok(PostgresFileHandle::new(self.pool.clone(), result.blob, result.id).await?)
    }
}

/// Postgres file storage backend.
pub struct PostgresBackend {
    pub pool: PgPool,
}

impl PostgresBackend {
    /// Runs migrations against the pool.
    pub async fn migrate(pool: PgPool) -> Result<(), BoxError> {
        // Verify that you can actually run a query at all (and that sqlx bits
        // are all working).
        let q = sqlx::query!("select 1 as foo");
        let _ = q.fetch_one(&pool).await?;

        MIGRATOR.run(&pool).await?;
        Ok(())
    }

    pub async fn create_bucket(
        &self,
        name: &str,
        default_ttl: Option<TimeDelta>,
    ) -> Result<PostgresBucket, FileCreateError> {
        PostgresBucket::create(self.pool.clone(), name, default_ttl).await
    }

    /// Constructs a PostgresBackend against the given pool. Does not run
    /// migrations (!).
    pub async fn new(pool: PgPool) -> Result<PostgresBackend, BoxError> {
        Ok(PostgresBackend { pool })
    }
}

#[async_trait]
impl StorageBackend for PostgresBackend {
    type Bucket<'a> = Arc<PostgresBucket>;

    #[tracing::instrument(level = "debug", skip(self))]
    async fn bucket(&self, name: &str) -> Result<Self::Bucket<'_>, FileOpenError> {
        // FIXME(jadel): name checking; not that it really matters, because of
        // the http framework already doing path normalization
        PostgresBucket::by_name(self.pool.clone(), name)
            .await?
            .ok_or(FileOpenError::DoesNotExist)
            .map(Arc::new)
    }

    #[tracing::instrument(level = "debug", skip(self))]
    async fn list_buckets(&self) -> Result<Vec<String>, BoxError> {
        Ok(PostgresBucket::list(self.pool.clone()).await?)
    }
}

/// Testing utilities for the postgres backend
pub mod testing {
    use std::ops::Deref;

    use sqlx::PgPool;
    use tmp_postgrust::TmpPostgrustFactory;

    use crate::BoxError;

    use super::PostgresBackend;

    static POSTGRES_FACTORY: tokio::sync::OnceCell<
        tokio::sync::Mutex<tmp_postgrust::TmpPostgrustFactory>,
    > = tokio::sync::OnceCell::const_new();

    pub struct Fixture {
        pub backend: PostgresBackend,
        _db: tmp_postgrust::asynchronous::ProcessGuard,
    }

    impl Deref for Fixture {
        type Target = PostgresBackend;

        fn deref(&self) -> &Self::Target {
            &self.backend
        }
    }

    impl Fixture {
        pub async fn new() -> Result<Fixture, BoxError> {
            // This is a workaround for a bug in tmp_postgrust where the
            // migrations are run in a synchronous callback from an async
            // thread.
            //
            // The solution to this is, erm, to run the async migrations as a
            // non-async, blocking, task on a tokio blocking-tasks thread.
            // https://github.com/CQCL/tmp-postgrust/issues/18
            let factory_mutex = POSTGRES_FACTORY
                .get_or_try_init(|| async {
                    let factory = TmpPostgrustFactory::try_new_async().await?;
                    let factory: TmpPostgrustFactory = tokio::task::spawn_blocking(move || {
                        factory.run_migrations(|conn_string| {
                            let runtime = tokio::runtime::Builder::new_current_thread()
                                .enable_all()
                                .build()
                                .unwrap();

                            let pool = PgPool::connect_lazy(conn_string)?;
                            runtime.block_on(PostgresBackend::migrate(pool))?;

                            Ok(())
                        })?;
                        Ok::<_, BoxError>(factory)
                    })
                    .await??;
                    Ok::<_, BoxError>(tokio::sync::Mutex::new(factory))
                })
                .await?;

            let db = {
                let factory = factory_mutex.lock().await;
                factory.new_instance_async().await?
            };

            let pool = PgPool::connect_lazy(&db.connection_string())?;
            let backend = PostgresBackend::new(pool).await?;

            Ok(Fixture { backend, _db: db })
        }

        pub async fn new_with_bucket() -> Result<Fixture, BoxError> {
            let fixture = Self::new().await?;
            fixture.create_bucket("bucket", None).await?;
            Ok(fixture)
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncReadExt;

    use super::testing::*;
    use super::*;

    #[tokio::test]
    async fn test_trivial() -> Result<(), BoxError> {
        let _fixture = Fixture::new().await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_lists_buckets() -> Result<(), BoxError> {
        let backend = Fixture::new().await?;

        assert!(backend.list_buckets().await?.is_empty());

        backend.create_bucket("bukkit", None).await?;

        assert_eq!(backend.list_buckets().await?, vec!["bukkit"]);
        Ok(())
    }

    #[tokio::test]
    async fn test_bucket_create_file_once() -> Result<(), BoxError> {
        let backend = Fixture::new_with_bucket().await?;

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
            handle.commit().await?;
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
    async fn test_directories() -> Result<(), BoxError> {
        let backend = Fixture::new_with_bucket().await?;

        let bucket = backend.bucket("bucket").await?;
        {
            let mut file = bucket.create_file("meow/kitty").await?;
            file.append(b"data").await?;
            file.commit().await?;
        }

        {
            let mut file = bucket.file("meow/kitty").await?;
            let mut string = String::new();
            file.read_to_string(&mut string).await?;
            assert_eq!(string, "data");
        }

        Ok(())
    }
}
