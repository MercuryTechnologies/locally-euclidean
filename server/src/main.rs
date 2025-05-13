mod tracing_setup;

use axum_tracing_opentelemetry::middleware::{OtelAxumLayer, OtelInResponseLayer};
use std::net::Ipv6Addr;
use tracing::info;

use axum::{BoxError, routing::get};

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    // FIXME(jadel): this feels so complex, idk if the
    // init_tracing_opentelemetry crate is the right thing for this. OTOH it
    // does the right thing.
    let _guard = tracing_setup::init_subscribers()?;

    let app = axum::Router::new()
        .route(
            "/",
            get(|| async { "This is https://github.com/MercuryTechnologies/locally-euclidean" }),
        )
        // Include trace context as header into the response
        .layer(OtelInResponseLayer::default())
        // Start OpenTelemetry trace on incoming request
        .layer(OtelAxumLayer::default())
        // Processed *outside* span
        .route("/healthcheck", get(|| async { "ok" }));

    let binding = (Ipv6Addr::LOCALHOST, 9000);
    info!("Listening on http://[{}]:{}", binding.0, binding.1);
    let listener = tokio::net::TcpListener::bind(binding).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
