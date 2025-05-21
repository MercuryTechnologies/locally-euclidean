pub mod api;
pub mod config;
pub mod explore;
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

/// Creates the app router.
pub fn make_app(state: AppState) -> axum::Router {
    // Note: Middleware is applied to existing routes only; routes added after
    // middleware will receive the middleware. In a similar fashion,
    // `with_state` only exposes the state to the routes already defined above
    // it.
    //
    // See: https://docs.rs/axum/0.8.4/axum/struct.Router.html#method.layer
    axum::Router::new()
        .route(
            "/",
            get(|| async { "This is https://github.com/MercuryTechnologies/locally-euclidean" }),
        )
        .nest("/v0", api::make_router())
        .nest("/explore", explore::make_router())
        .layer(
            // Process middleware requests top to bottom then responses are
            // modified bottom to top (unlike .layer() which runs middleware on
            // requests bottom to top then on responses top to bottom). See the
            // diagram in
            // https://docs.rs/axum/0.8.4/axum/middleware/index.html for
            // details.
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
