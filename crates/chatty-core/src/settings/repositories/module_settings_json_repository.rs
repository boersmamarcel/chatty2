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
