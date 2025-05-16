pub mod api;
pub mod config;
mod security_headers;
pub mod tracing_setup;

use std::sync::Arc;

use axum_tracing_opentelemetry::middleware::{OtelAxumLayer, OtelInResponseLayer};

use axum::routing::get;
use config::AppConfig;
use storage::file_backend::FileBackend;
use tower::ServiceBuilder;

pub type AppState = Arc<AppStateInner>;

pub struct AppStateInner {
    pub config: AppConfig,
    pub store: FileBackend,
}

impl AppStateInner {
    pub fn new(config: AppConfig) -> AppState {
        Arc::new(AppStateInner {
            store: FileBackend::new(config.file_storage_root.clone()),
            config,
        })
    }
}

pub fn make_app(state: AppState) -> axum::Router {
    axum::Router::new()
        .route(
            "/",
            get(|| async { "This is https://github.com/MercuryTechnologies/locally-euclidean" }),
        )
        .nest("/v0", api::make_router())
        .layer(
            ServiceBuilder::new()
                // Start OpenTelemetry trace on incoming request
                .layer(OtelAxumLayer::default())
                // Include trace context as header into the response
                .layer(OtelInResponseLayer)
                .layer(security_headers::cors())
                .layer(axum::middleware::from_fn(security_headers::headers)),
        )
        .with_state(state)
        // Processed *outside* span
        .route("/healthcheck", get(|| async { "ok" }))
}
