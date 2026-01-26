use std::path::PathBuf;

use super::general_settings_repository::{BoxFuture, GeneralSettingsRepository};
use super::provider_repository::{RepositoryError, RepositoryResult};
use crate::settings::models::general_model::GeneralSettingsModel;

pub struct GeneralSettingsJsonRepository {
    file_path: PathBuf,
}

impl GeneralSettingsJsonRepository {
    /// Create repository with XDG-compliant path
    pub fn new() -> RepositoryResult<Self> {
        let config_dir = dirs::config_dir().ok_or_else(|| {
            RepositoryError::PathError("Cannot determine config directory".into())
        })?;

        let app_dir = config_dir.join("chatty");
        let file_path = app_dir.join("general_settings.json");

        Ok(Self { file_path })
    }
}

impl GeneralSettingsRepository for GeneralSettingsJsonRepository {
    fn load(&self) -> BoxFuture<'static, RepositoryResult<GeneralSettingsModel>> {
        let path = self.file_path.clone();

        Box::pin(async move {
            // Check if file exists
            if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
                return Ok(GeneralSettingsModel::default());
            }

            // Read file
            let contents = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            // Parse JSON
            let settings: GeneralSettingsModel = serde_json::from_str(&contents)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            Ok(settings)
        })
    }

    fn save(&self, settings: GeneralSettingsModel) -> BoxFuture<'static, RepositoryResult<()>> {
        let path = self.file_path.clone();

        Box::pin(async move {
            // Serialize to JSON
            let json = serde_json::to_string_pretty(&settings)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            // Create directory if needed
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| RepositoryError::IoError(e.to_string()))?;
            }

            // Write atomically using temp file + rename
            let temp_path = path.with_extension("json.tmp");
            tokio::fs::write(&temp_path, &json)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            tokio::fs::rename(&temp_path, &path)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            Ok(())
        })
    }
}
