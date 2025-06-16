//! Automated idempotent maintenance tasks.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::AppState;

/// Runs all the async maintenance jobs
pub async fn start_maintenance_tasks(
    app_state: AppState,
    terminate: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn({
        let app_state = app_state.clone();
        let terminate = terminate.clone();
        let mut interval = tokio::time::interval(Duration::from_secs(120));
        async move {
            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    _ = terminate.cancelled() => {}
                }
                if terminate.is_cancelled() {
                    break;
                }

                let result = app_state.store.delete_old_files_batch().await;
                if let Err(err) = result {
                    tracing::error!(err, "Error while deleting old files");
                }
            }
        }
    })
}
