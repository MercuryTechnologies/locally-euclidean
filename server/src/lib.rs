pub mod api;
pub mod config;
mod errors;
pub mod explore;
mod security_headers;
pub mod tracing_setup;

use std::sync::Arc;

use axum_tracing_opentelemetry::middleware::{OtelAxumLayer, OtelInResponseLayer};

use axum::routing::get;
use config::AppConfig;
use sqlx::PgPool;
use storage::postgres::PostgresBackend;
use tower::ServiceBuilder;

pub type AppState = Arc<AppStateInner>;

pub struct AppStateInner {
    pub config: AppConfig,
    pub pool: PgPool,
    pub store: PostgresBackend,
}

#[derive(thiserror::Error, Debug)]
pub enum AppStartupError {
    #[error("Failed to connect to database: {0}")]
    ConnectToDB(sqlx::Error),
    #[error("Failed to run migrations: {0}")]
    Migrate(sqlx::Error),
}

impl AppStateInner {
    /// Used for tests: start up the app with injected backend and database.
    pub fn with_backend_migrated(
        config: AppConfig,
        pool: PgPool,
        store: PostgresBackend,
    ) -> AppState {
        Arc::new(AppStateInner {
            store,
            pool,
            config,
        })
    }

    /// Start up the app and migrate the DB.
    pub async fn new(config: AppConfig) -> Result<AppState, AppStartupError> {
        let pool = sqlx::PgPool::connect_lazy(&config.db_connection_string)
            .map_err(AppStartupError::ConnectToDB)?;
        PostgresBackend::migrate(pool.clone())
            .await
            .map_err(AppStartupError::Migrate)?;
        let store = PostgresBackend::new(pool.clone());

        Ok(Self::with_backend_migrated(config, pool, store))
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
        .nest("/v1/logs", api::make_buck2_logs_router())
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
                .layer(axum::middleware::map_response(security_headers::headers)),
        )
        .with_state(state)
        // Processed *outside* span
        .route("/healthcheck", get(|| async { "ok" }))
}
