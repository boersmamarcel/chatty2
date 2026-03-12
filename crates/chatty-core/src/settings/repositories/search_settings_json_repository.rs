use std::path::PathBuf;

use super::provider_repository::{RepositoryError, RepositoryResult};
use super::search_settings_repository::{BoxFuture, SearchSettingsRepository};
use crate::settings::models::search_settings::SearchSettingsModel;

pub struct SearchSettingsJsonRepository {
    file_path: PathBuf,
}

impl SearchSettingsJsonRepository {
    /// Create repository with XDG-compliant path
    pub fn new() -> RepositoryResult<Self> {
        let config_dir = dirs::config_dir().ok_or_else(|| {
            RepositoryError::PathError("Cannot determine config directory".into())
        })?;

        let app_dir = config_dir.join("chatty");
        let file_path = app_dir.join("search_settings.json");

        Ok(Self { file_path })
    }
}

impl SearchSettingsRepository for SearchSettingsJsonRepository {
    fn load(&self) -> BoxFuture<'static, RepositoryResult<SearchSettingsModel>> {
        let path = self.file_path.clone();

        Box::pin(async move {
            if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
                return Ok(SearchSettingsModel::default());
            }

            let contents = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            let settings: SearchSettingsModel = serde_json::from_str(&contents)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            Ok(settings)
        })
    }

    fn save(&self, settings: SearchSettingsModel) -> BoxFuture<'static, RepositoryResult<()>> {
        let path = self.file_path.clone();

        Box::pin(async move {
            let json = serde_json::to_string_pretty(&settings)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| RepositoryError::IoError(e.to_string()))?;
            }

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
