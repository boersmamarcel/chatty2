use std::path::PathBuf;

use super::provider_repository::{RepositoryError, RepositoryResult};
use super::training_settings_repository::{BoxFuture, TrainingSettingsRepository};
use crate::settings::models::training_settings::TrainingSettingsModel;

pub struct TrainingSettingsJsonRepository {
    file_path: PathBuf,
}

impl TrainingSettingsJsonRepository {
    /// Create repository with XDG-compliant path
    pub fn new() -> RepositoryResult<Self> {
        let config_dir = dirs::config_dir().ok_or_else(|| {
            RepositoryError::PathError("Cannot determine config directory".into())
        })?;

        let app_dir = config_dir.join("chatty");
        let file_path = app_dir.join("training_settings.json");

        Ok(Self { file_path })
    }
}

impl TrainingSettingsRepository for TrainingSettingsJsonRepository {
    fn load(&self) -> BoxFuture<'static, RepositoryResult<TrainingSettingsModel>> {
        let path = self.file_path.clone();

        Box::pin(async move {
            // Check if file exists
            if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
                return Ok(TrainingSettingsModel::default());
            }

            // Read file
            let contents = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            // Parse JSON
            let settings: TrainingSettingsModel = serde_json::from_str(&contents)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            Ok(settings)
        })
    }

    fn save(&self, settings: TrainingSettingsModel) -> BoxFuture<'static, RepositoryResult<()>> {
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

            // Write atomically using temp file + rename.
            let temp_path = path.with_extension(format!("json.{}.tmp", std::process::id()));
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
