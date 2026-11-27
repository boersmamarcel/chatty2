use super::generic_json_repository::GenericJsonRepository;
use super::module_settings_repository::ModuleSettingsRepository;
use super::provider_repository::{BoxFuture, RepositoryError, RepositoryResult};
use crate::settings::models::module_settings::{ModuleSettingsModel, normalize_module_dir};

pub struct ModuleSettingsJsonRepository {
    inner: GenericJsonRepository<ModuleSettingsModel>,
}

impl ModuleSettingsJsonRepository {
    /// Create repository with XDG-compliant path.
    pub fn new() -> RepositoryResult<Self> {
        Ok(Self {
            inner: GenericJsonRepository::new("module_settings.json")?,
        })
    }
}

/// Test-only constructor for a custom file path (used by unit tests and the
/// `store_conformance` suite exported behind `test-support`).
#[cfg(any(test, feature = "test-support"))]
impl ModuleSettingsJsonRepository {
    pub fn with_path(file_path: std::path::PathBuf) -> Self {
        Self {
            inner: GenericJsonRepository::with_path(file_path),
        }
    }
}

impl ModuleSettingsRepository for ModuleSettingsJsonRepository {
    fn load(&self) -> BoxFuture<'static, RepositoryResult<ModuleSettingsModel>> {
        let path = self.inner.file_path().to_path_buf();

        Box::pin(async move {
            if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
                return Ok(ModuleSettingsModel::default());
            }

            let contents = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            let mut value: serde_json::Value = serde_json::from_str(&contents)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;
            // `gateway_port` was dropped in v0.6.0. A file that still has it
            // is migrated, not refused: the key goes and the file is written
            // back, so nobody gets stuck on an old settings file (AGE-777).
            if let Some(object) = value.as_object_mut()
                && object.remove("gateway_port").is_some()
            {
                tracing::info!(
                    path = %path.display(),
                    "Removed the old gateway_port setting (dropped in v0.6.0)"
                );
                let migrated = serde_json::to_string_pretty(&value)
                    .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;
                let tmp = path.with_extension("json.migrate.tmp");
                tokio::fs::write(&tmp, migrated)
                    .await
                    .map_err(|e| RepositoryError::IoError(e.to_string()))?;
                tokio::fs::rename(&tmp, &path)
                    .await
                    .map_err(|e| RepositoryError::IoError(e.to_string()))?;
            }

            let mut settings: ModuleSettingsModel = serde_json::from_value(value)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            // Normalize the module directory path after loading.
            let normalized_dir = normalize_module_dir(settings.module_dir.clone());
            if normalized_dir != settings.module_dir {
                settings.module_dir = normalized_dir;
            }

            Ok(settings)
        })
    }

    fn save(&self, settings: ModuleSettingsModel) -> BoxFuture<'static, RepositoryResult<()>> {
        self.inner.save(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An old file that still has `gateway_port` loads: the key is removed
    /// and the file written back without it (AGE-777).
    #[tokio::test]
    async fn old_settings_with_gateway_port_are_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("module_settings.json");
        std::fs::write(&path, r#"{"enabled":true,"gateway_port":8420}"#).unwrap();

        let repo = ModuleSettingsJsonRepository::with_path(path.clone());
        let settings = repo.load().await.expect("an old file loads");

        assert!(settings.enabled, "the other settings are kept");
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(!written.contains("gateway_port"), "key removed: {written}");
        assert!(
            written.contains("\"enabled\": true"),
            "rest kept: {written}"
        );
    }

    /// Unknown fields are refused by `deny_unknown_fields`.
    #[tokio::test]
    async fn other_unknown_settings_keys_still_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("module_settings.json");
        std::fs::write(&path, r#"{"enabled":true,"some_made_up_field":123}"#).unwrap();

        let repo = ModuleSettingsJsonRepository::with_path(path);
        let err = repo.load().await.unwrap_err();

        let msg = err.to_string();
        assert!(
            msg.contains("some_made_up_field"),
            "expected the unknown-field error to name the field: {msg}"
        );
    }
}
