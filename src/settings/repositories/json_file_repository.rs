use std::path::PathBuf;

use super::provider_repository::{
    BoxFuture, ProviderRepository, RepositoryError, RepositoryResult,
};
use crate::settings::models::providers_store::ProviderConfig;

pub struct JsonFileRepository {
    file_path: PathBuf,
}

impl JsonFileRepository {
    /// Create repository with XDG-compliant path
    pub fn new() -> RepositoryResult<Self> {
        let config_dir = dirs::config_dir().ok_or_else(|| {
            RepositoryError::PathError("Cannot determine config directory".into())
        })?;

        let app_dir = config_dir.join("chatty");
        let file_path = app_dir.join("providers.json");

        Ok(Self { file_path })
    }

    /// Create repository with custom path (for testing)
    pub fn with_path(file_path: PathBuf) -> Self {
        Self { file_path }
    }
}

impl ProviderRepository for JsonFileRepository {
    fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<ProviderConfig>>> {
        let path = self.file_path.clone();

        Box::pin(async move {
            // If file doesn't exist, return empty list (first run)
            if !path.exists() {
                return Ok(Vec::new());
            }

            let contents = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            let configs: Vec<ProviderConfig> = serde_json::from_str(&contents)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            Ok(configs)
        })
    }

    fn save_all(&self, providers: Vec<ProviderConfig>) -> BoxFuture<'static, RepositoryResult<()>> {
        let path = self.file_path.clone();

        Box::pin(async move {
            // Ensure directory exists first
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| RepositoryError::IoError(e.to_string()))?;
            }

            // Serialize directly to JSON
            let json = serde_json::to_string_pretty(&providers)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            // Write atomically using temp file + rename
            let temp_path = path.with_extension("json.tmp");
            tokio::fs::write(&temp_path, json)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            tokio::fs::rename(&temp_path, &path)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            Ok(())
        })
    }

    fn storage_path(&self) -> String {
        self.file_path.to_string_lossy().to_string()
    }
}
