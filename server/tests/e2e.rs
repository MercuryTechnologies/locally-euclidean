//! End to end test of locally_euclidean
use axum::BoxError;
use axum_test::TestServer;
use server::{AppStateInner, config::AppConfig, make_app};

/// Set-up for a test with isolated storage directory
struct Fixture {
    _temp_dir: tempfile::TempDir,
    server: TestServer,
}

impl Fixture {
    fn new() -> Result<Fixture, BoxError> {
        let (temp_dir, config) = AppConfig::build_for_test()?;
        // Create one bucket
        std::fs::create_dir(temp_dir.path().join("my_bucket"))?;

        let app = make_app(AppStateInner::new(config));
        Ok(Fixture {
            _temp_dir: temp_dir,
            server: TestServer::new(app)?,
        })
    }
}

#[tokio::test]
#[ignore = "TODO"]
async fn put_write_filename() -> Result<(), BoxError> {
    let f = Fixture::new()?;
    f.server
        .put("/v0/write/meowmeow?bucketName=my_bucket")
        .text("meow!")
        .expect_success()
        .await;

    // Cannot write twice
    f.server
        .put("/v0/write/meowmeow?bucketName=my_bucket")
        .text("meow!")
        .expect_failure()
        .await;
    Ok(())
}
