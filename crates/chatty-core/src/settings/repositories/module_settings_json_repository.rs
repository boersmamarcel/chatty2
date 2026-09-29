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

            // One-time drop of the obsolete `gateway_port` key (AGE-768):
            // `deny_unknown_fields` would otherwise refuse an existing
            // user's file that still has it, breaking the TUI and the
            // desktop plugin settings on upgrade. Only this exact key is
            // special-cased; every other unknown field is still refused.
            // Remove this exception in v0.6.0 (AGE-777).
            let mut had_gateway_port = false;
            let contents = match serde_json::from_str::<serde_json::Value>(&contents) {
                Ok(serde_json::Value::Object(mut map)) if map.contains_key("gateway_port") => {
                    map.remove("gateway_port");
                    had_gateway_port = true;
                    serde_json::to_string(&serde_json::Value::Object(map))
                        .map_err(|e| RepositoryError::SerializationError(e.to_string()))?
                }
                _ => contents,
            };

            let mut settings: ModuleSettingsModel = serde_json::from_str(&contents)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            // Normalize the module directory path after loading.
            let normalized_dir = normalize_module_dir(settings.module_dir.clone());
            if normalized_dir != settings.module_dir {
                settings.module_dir = normalized_dir;
            }

            if had_gateway_port {
                tracing::info!(
                    "dropping obsolete gateway_port from module_settings.json; the gateway now uses an owner-only socket"
                );
                super::generic_json_repository::write_atomic(&path, &settings).await?;
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

    /// AGE-768: an old file that still has `gateway_port` loads instead of
    /// failing `deny_unknown_fields`, the key is gone from the model, and
    /// the file on disk is rewritten without it (so the drop truly only
    /// happens once).
    #[tokio::test]
    async fn old_settings_with_gateway_port_load_and_drop_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("module_settings.json");
        std::fs::write(
            &path,
            r#"{"enabled":true,"gateway_port":8420,"module_dir":"/tmp/modules"}"#,
        )
        .unwrap();

        let repo = ModuleSettingsJsonRepository::with_path(path.clone());
        let settings = repo.load().await.unwrap();

        assert!(settings.enabled);
        assert_eq!(settings.module_dir, "/tmp/modules");

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            !on_disk.contains("gateway_port"),
            "gateway_port should have been dropped from the saved file: {on_disk}"
        );

        // Loading again (the key is now gone) still succeeds and needs no
        // further rewrite.
        let settings_again = repo.load().await.unwrap();
        assert!(settings_again.enabled);
    }

    /// Only the exact `gateway_port` key is special-cased; any other
    /// unknown field is still refused by `deny_unknown_fields`.
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
