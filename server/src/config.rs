use std::path::PathBuf;

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

    /// Root directory of the file storage
    pub file_storage_root: PathBuf,
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

    /// Creates a testing AppConfig that stores files in a temporary directory.
    pub fn build_for_test() -> Result<(tempfile::TempDir, AppConfig), BoxError> {
        let temp_dir = tempfile::TempDir::with_suffix("locally-euclidean-test")?;
        let file_storage_root = temp_dir.path().to_path_buf();
        Ok((
            temp_dir,
            AppConfig {
                max_upload_size_mb: 5,
                allowed_content_types: Self::default_allowed_content_types(),
                file_storage_root,
            },
        ))
    }

    pub fn build() -> Result<AppConfig, ConfigBuildError> {
        let config_unparsed = ::config::Config::builder()
            // e.g. LOC_EUC_FILE_STORAGE_ROOT
            .add_source(::config::Environment::with_prefix("LOC_EUC"))
            .build()
            .map_err(ConfigBuildError::FailedToCollect)?;

        config_unparsed
            .try_deserialize()
            .map_err(ConfigBuildError::FailedToDeserialize)
    }
}
