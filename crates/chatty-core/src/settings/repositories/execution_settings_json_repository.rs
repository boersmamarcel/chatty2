use super::execution_settings_repository::ExecutionSettingsRepository;
use super::generic_json_repository::GenericJsonRepository;
use super::provider_repository::{BoxFuture, RepositoryResult};
use crate::settings::models::execution_settings::ExecutionSettingsModel;

pub struct ExecutionSettingsJsonRepository {
    inner: GenericJsonRepository<ExecutionSettingsModel>,
}

impl ExecutionSettingsJsonRepository {
    /// Create repository with XDG-compliant path
    pub fn new() -> RepositoryResult<Self> {
        Ok(Self {
            inner: GenericJsonRepository::new("execution_settings.json")?,
        })
    }
}

impl ExecutionSettingsRepository for ExecutionSettingsJsonRepository {
    fn load(&self) -> BoxFuture<'static, RepositoryResult<ExecutionSettingsModel>> {
        self.inner.load()
    }

    fn save(&self, settings: ExecutionSettingsModel) -> BoxFuture<'static, RepositoryResult<()>> {
        self.inner.save(settings)
    }
}
