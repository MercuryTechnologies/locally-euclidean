use clap::Parser as _;
use futures::future::FutureExt;
use miette::Context;
use miette::IntoDiagnostic;
use miette::Result;
use miette::miette;
use server::AppStateInner;
use server::config::AppConfig;
use server::make_app;
use server::tasks;
use server::tracing_setup;
use tokio_util::sync::CancellationToken;
use tracing::info;

fn parse_timedelta(input: &str) -> Result<chrono::TimeDelta, humantime::DurationError> {
    humantime::parse_duration(input)
        .map(|duration| chrono::TimeDelta::from_std(duration).expect("duration out of range"))
}

/// Create a bucket.
#[derive(clap::Args)]
struct CreateBucketArgs {
    /// Name of the bucket.
    name: String,
    /// How long before files should be deleted, if the client doesn't specify
    /// a TTL themselves.
    #[clap(value_parser = parse_timedelta)]
    default_ttl: Option<chrono::TimeDelta>,
}

#[derive(clap::Subcommand)]
enum MaintenanceTask {
    CreateBucket(CreateBucketArgs),
}

/// Perform some offline maintenance task against the locally-euclidean
/// instance.
#[derive(clap::Parser)]
struct Maintenance {
    #[clap(subcommand)]
    task: MaintenanceTask,
}

#[derive(clap::Parser)]
enum Subcommand {
    /// Run the service.
    Serve,
    Maintenance(Maintenance),
}

#[derive(clap::Parser)]
struct Args {
    #[clap(subcommand)]
    subcommand: Subcommand,
}

async fn run_maintenance(config: AppConfig, maintenance: Maintenance) -> Result<()> {
    let state = AppStateInner::new(config).await?;

    match maintenance.task {
        MaintenanceTask::CreateBucket(CreateBucketArgs { name, default_ttl }) => {
            let bucket = state
                .store
                .create_bucket(&name, default_ttl)
                .await
                .into_diagnostic()
                .wrap_err("Creating the bucket failed")?;
            tracing::info!("Created: {bucket:?}");
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // FIXME(jadel): this feels so complex, idk if the
    // init_tracing_opentelemetry crate is the right thing for this. OTOH it
    // does the right thing.
    let _guard = tracing_setup::init_subscribers()
        .map_err(|e| miette!(e))
        .wrap_err("Tracing setup failed")?;

    let args = Args::parse();
    let config = AppConfig::build()?;

    match args.subcommand {
        Subcommand::Serve => {
            let state = AppStateInner::new(config).await?;
            let app = make_app(state.clone());

            let terminate = CancellationToken::new();
            tokio::spawn({
                let interrupted = terminate.clone();
                async move {
                    tokio::signal::ctrl_c()
                        .await
                        .expect("failed to listen for ctrl-c, wat");
                    interrupted.cancel();
                }
            });

            info!("Starting scheduled maintenance tasks");
            let tasks_fut = tasks::start_maintenance_tasks(state.clone(), terminate.clone()).await;

            info!("Listening on http://{}", state.config.bind_address);
            let listener = tokio::net::TcpListener::bind(state.config.bind_address)
                .await
                .into_diagnostic()
                .wrap_err("Failed to create listener")?;

            let listener_fut = axum::serve(listener, app)
                .with_graceful_shutdown(terminate.clone().cancelled_owned());

            tokio::try_join!(
                async {
                    let res = listener_fut.await;
                    // Failing to start the listener means we should stop
                    // altogether.
                    if res.is_err() {
                        terminate.cancel();
                    }
                    res.into_diagnostic().wrap_err("Failed to listen")
                },
                tasks_fut.into_future().map(|res| res
                    .into_diagnostic()
                    .wrap_err("Failed to join background tasks"))
            )?;
        }
        Subcommand::Maintenance(maintenance) => run_maintenance(config, maintenance).await?,
    };

    Ok(())
}
