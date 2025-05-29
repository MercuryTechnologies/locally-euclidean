//! End to end test of locally_euclidean
use axum::BoxError;
use axum_test::TestServer;
use server::{AppStateInner, config::AppConfig, make_app};

/// Set-up for a test with isolated storage directory
struct Fixture {
    temp_dir: tempfile::TempDir,
    server: TestServer,
}

impl Fixture {
    pub fn create_bucket(&self, name: &str) -> Result<(), BoxError> {
        // Create one bucket
        std::fs::create_dir(self.temp_dir.path().join(name))?;
        Ok(())
    }

    pub fn new() -> Result<Fixture, BoxError> {
        let (temp_dir, config) = AppConfig::build_for_test()?;

        let app = make_app(AppStateInner::new(config));
        let fixture = Fixture {
            temp_dir,
            server: TestServer::new(app)?,
        };
        fixture.create_bucket("my_bucket")?;
        Ok(fixture)
    }
}

#[tokio::test]
async fn put_write_filename() -> Result<(), BoxError> {
    let f = Fixture::new()?;
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
    let f = Fixture::new()?;
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
    let f = Fixture::new()?;
    f.create_bucket("buck2_logs")?;
    f.server
        .put("/v0/write/flat/abcde.pb.zst?bucketName=buck2_logs")
        .text("meow!")
        .expect_success()
        .await;

    let resp = f.server.get("/v1/logs/get/abcde").expect_success().await;
    resp.assert_text("meow!");

    Ok(())
}
