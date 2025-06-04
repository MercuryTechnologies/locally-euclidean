use axum::BoxError;
use clap::Parser as _;
use server::AppStateInner;
use server::config::AppConfig;
use server::make_app;
use server::tracing_setup;
use std::net::Ipv6Addr;
use tracing::info;

#[derive(clap::Parser)]
enum Subcommand {
    Serve,
}

#[derive(clap::Parser)]
struct Args {
    #[clap(subcommand)]
    subcommand: Subcommand,
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    // FIXME(jadel): this feels so complex, idk if the
    // init_tracing_opentelemetry crate is the right thing for this. OTOH it
    // does the right thing.
    let _guard = tracing_setup::init_subscribers()?;

    let args = Args::parse();
    let config = AppConfig::build()?;

    match args.subcommand {
        Subcommand::Serve => {
            let app = make_app(AppStateInner::new(config).await?);

            let binding = (Ipv6Addr::LOCALHOST, 9000);
            info!("Listening on http://[{}]:{}", binding.0, binding.1);
            let listener = tokio::net::TcpListener::bind(binding).await?;
            axum::serve(listener, app).await?;
        }
    };

    Ok(())
}
