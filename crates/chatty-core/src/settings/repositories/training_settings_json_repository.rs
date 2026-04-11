use super::generic_json_repository::GenericJsonRepository;
use super::provider_repository::{BoxFuture, RepositoryResult};
use super::training_settings_repository::TrainingSettingsRepository;
use crate::settings::models::training_settings::TrainingSettingsModel;

pub struct TrainingSettingsJsonRepository {
    inner: GenericJsonRepository<TrainingSettingsModel>,
}

impl TrainingSettingsJsonRepository {
    /// Create repository with XDG-compliant path
    pub fn new() -> RepositoryResult<Self> {
        Ok(Self {
            inner: GenericJsonRepository::new("training_settings.json")?,
        })
    }
}

impl TrainingSettingsRepository for TrainingSettingsJsonRepository {
    fn load(&self) -> BoxFuture<'static, RepositoryResult<TrainingSettingsModel>> {
        self.inner.load()
    }

    fn save(&self, settings: TrainingSettingsModel) -> BoxFuture<'static, RepositoryResult<()>> {
        self.inner.save(settings)
    }
}
