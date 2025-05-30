//! Postgres storage backend.
//!
//! This is *all* postgres: data is stored in blobs.

use sqlx::PgPool;

use crate::BoxError;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../migrations");

/// Postgres file storage backend.
pub struct PostgresBackend {
    _pool: PgPool,
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

    /// Constructs a PostgresBackend against the given pool. Does not run
    /// migrations (!).
    pub async fn new(pool: PgPool) -> Result<PostgresBackend, BoxError> {
        Ok(PostgresBackend { _pool: pool })
    }
}

/// Testing utilities for the postgres backend
pub mod testing {
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

    impl Fixture {
        pub async fn new() -> Result<Fixture, BoxError> {
            // This is a workaround for a bug in tmp_postgrust where the
            // migraitons are run in a synchronous callback from an async
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
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    #[tokio::test]
    async fn test_trivial() -> Result<(), BoxError> {
        let _fixture = Fixture::new().await?;

        Ok(())
    }
}
