//! End to end test of locally_euclidean
use axum::BoxError;
use axum_test::TestServer;
use chrono::TimeDelta;
use server::{AppState, AppStateInner, config::AppConfig, make_app};
use storage::{
    FileCreateError,
    postgres::{PostgresBucket, testing},
};

/// Set-up for a test with isolated database
struct Fixture {
    server: TestServer,
    state: AppState,
    _guard: testing::Guard,
}

impl Fixture {
    pub async fn create_bucket(
        &self,
        name: &str,
        default_ttl: Option<TimeDelta>,
    ) -> Result<PostgresBucket, FileCreateError> {
        PostgresBucket::create(self.state.pool.clone(), name, default_ttl).await
    }

    pub async fn new() -> Result<Fixture, BoxError> {
        let config = AppConfig::build_for_test()?;
        let storage_fixture = testing::Fixture::new().await?;
        storage_fixture.create_bucket("my_bucket", None).await?;

        // Needs to be torn apart because of needing to be an AppState instead.
        let testing::Fixture { backend, db } = storage_fixture;

        let state = AppStateInner::with_backend_migrated(config, backend.pool.clone(), backend);
        let app = make_app(state.clone());
        let fixture = Fixture {
            server: TestServer::new(app)?,
            state,
            _guard: db,
        };
        Ok(fixture)
    }
}

#[tokio::test]
async fn put_write_filename() -> Result<(), BoxError> {
    let f = Fixture::new().await?;
    f.server
        .put("/v0/write/meowmeow?bucketName=my_bucket")
        .text("meow!")
        .expect_success()
        .await;

    // Can write twice if it's idempotent
    f.server
        .put("/v0/write/meowmeow?bucketName=my_bucket")
        .text("meow!")
        .expect_success()
        .await;

    // But can't overwrite files
    let resp = f
        .server
        .put("/v0/write/meowmeow?bucketName=my_bucket")
        .text("kitty")
        .expect_failure()
        .await;
    resp.assert_status_conflict();
    resp.assert_text("File already exists with conflicting content");
    Ok(())
}

#[tokio::test]
async fn post_append_filename() -> Result<(), BoxError> {
    let f = Fixture::new().await?;
    // Can't append to a file that doesn't exist
    let resp = f
        .server
        .post("/v0/append/meowmeow?bucketName=my_bucket&writeOffset=0")
        .text("meow!")
        .expect_failure()
        .await;
    resp.assert_text("File does not exist: \"meowmeow\"");
    resp.assert_status_not_found();

    // Appending to a file that exists works
    f.server
        .put("/v0/write/meowmeow?bucketName=my_bucket")
        .text("meow!")
        .expect_success()
        .await;
    f.server
        .post("/v0/append/meowmeow?bucketName=my_bucket&writeOffset=6")
        .text("meow!")
        .expect_success()
        .await;

    f.server
        .get("/explore/my_bucket/meowmeow")
        .expect_success()
        .await
        .assert_text("meow!meow!");

    Ok(())
}

#[tokio::test]
async fn get_buck2_log() -> Result<(), BoxError> {
    let f = Fixture::new().await?;
    f.create_bucket("buck2_logs", None).await?;
    f.server
        .put("/v0/write/flat/abcde.pb.zst?bucketName=buck2_logs")
        .text("meow!")
        .expect_success()
        .await;

    let resp = f.server.get("/v1/logs/get/abcde").expect_success().await;
    resp.assert_text("meow!");

    Ok(())
}
