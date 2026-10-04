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

            let mut settings: ModuleSettingsModel = serde_json::from_str(&contents)
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

    /// An old file that still has `gateway_port` is refused like any other
    /// unknown key, with an error naming it (AGE-777).
    #[tokio::test]
    async fn old_settings_with_gateway_port_are_refused_naming_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("module_settings.json");
        std::fs::write(&path, r#"{"enabled":true,"gateway_port":8420}"#).unwrap();

        let repo = ModuleSettingsJsonRepository::with_path(path.clone());
        let msg = repo.load().await.unwrap_err().to_string();

        assert!(
            msg.contains("gateway_port"),
            "error should name the key: {msg}"
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("gateway_port")
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
