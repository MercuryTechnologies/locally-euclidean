use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};

use axum::BoxError;

#[derive(Debug, thiserror::Error)]
pub enum ConfigBuildError {
    #[error("Failed to collect config items: {0}")]
    FailedToCollect(::config::ConfigError),
    #[error("Failed to deserialize config file: {0}")]
    FailedToDeserialize(::config::ConfigError),
}

#[derive(Debug, serde::Deserialize)]
pub struct AppConfig {
    /// Maximum size of uploads. Uploads with longer length are rejected as bad
    /// requests as soon as we know (either because we have a too-high content
    /// length hint or because we actually received more than the limit).
    #[serde(default = "AppConfig::default_max_upload_size")]
    pub max_upload_size_mb: u64,

    /// Permitted Content-Types that may be uploaded to this service.
    ///
    /// FIXME(jadel): does this need a regex matcher?
    #[serde(default = "AppConfig::default_allowed_content_types")]
    pub allowed_content_types: Vec<String>,

    /// Bind address for the server.
    ///
    /// NOTE: the default only allows connections from localhost! This may or
    /// may not be a problem depending on setup.
    ///
    /// This is `[::1]:9000`.
    #[serde(default = "AppConfig::default_bind_address")]
    pub bind_address: SocketAddr,

    /// Database connection string for a Postgres database [according to sqlx](https://docs.rs/sqlx/0.8.6/sqlx/postgres/struct.PgConnectOptions.html).
    pub db_connection_string: String,
}

impl AppConfig {
    fn default_max_upload_size() -> u64 {
        100
    }

    fn default_allowed_content_types() -> Vec<String> {
        vec![
            "application/octet-stream".to_owned(),
            "text/plain".to_owned(),
            "text/html".to_owned(),
        ]
    }

    fn default_bind_address() -> SocketAddr {
        SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 9000, 0, 0))
    }

    /// Creates a testing AppConfig. Database sold separately.
    pub fn build_for_test() -> Result<AppConfig, BoxError> {
        Ok(AppConfig {
            max_upload_size_mb: 5,
            allowed_content_types: Self::default_allowed_content_types(),
            bind_address: Self::default_bind_address(),
            // Garbage value, not actually used
            db_connection_string: "".to_owned(),
        })
    }

    pub fn build() -> Result<AppConfig, ConfigBuildError> {
        let config_unparsed = ::config::Config::builder()
            .add_source(
                ::config::File::new("locally-euclidean.toml", ::config::FileFormat::Toml)
                    .required(false),
            )
            // e.g. LOC_EUC_FILE_STORAGE_ROOT
            .add_source(::config::Environment::with_prefix("LOC_EUC"))
            .build()
            .map_err(ConfigBuildError::FailedToCollect)?;

        config_unparsed
            .try_deserialize()
            .map_err(ConfigBuildError::FailedToDeserialize)
    }
}
