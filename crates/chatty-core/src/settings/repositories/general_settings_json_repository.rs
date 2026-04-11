use super::general_settings_repository::GeneralSettingsRepository;
use super::generic_json_repository::GenericJsonRepository;
use super::provider_repository::{BoxFuture, RepositoryResult};
use crate::settings::models::general_model::GeneralSettingsModel;

pub struct GeneralSettingsJsonRepository {
    inner: GenericJsonRepository<GeneralSettingsModel>,
}

impl GeneralSettingsJsonRepository {
    /// Create repository with XDG-compliant path
    pub fn new() -> RepositoryResult<Self> {
        Ok(Self {
            inner: GenericJsonRepository::new("general_settings.json")?,
        })
    }
}

impl GeneralSettingsRepository for GeneralSettingsJsonRepository {
    fn load(&self) -> BoxFuture<'static, RepositoryResult<GeneralSettingsModel>> {
        self.inner.load()
    }

    fn save(&self, settings: GeneralSettingsModel) -> BoxFuture<'static, RepositoryResult<()>> {
        self.inner.save(settings)
    }
}
