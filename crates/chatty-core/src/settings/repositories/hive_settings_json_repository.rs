use super::generic_json_repository::GenericJsonRepository;
use super::hive_settings_repository::HiveSettingsRepository;
use super::provider_repository::{BoxFuture, RepositoryResult};
use crate::settings::models::hive_settings::HiveSettingsModel;

pub struct HiveSettingsJsonRepository {
    inner: GenericJsonRepository<HiveSettingsModel>,
}

impl HiveSettingsJsonRepository {
    pub fn new() -> RepositoryResult<Self> {
        Ok(Self {
            inner: GenericJsonRepository::new("hive_settings.json")?,
        })
    }
}

impl HiveSettingsRepository for HiveSettingsJsonRepository {
    fn load(&self) -> BoxFuture<'static, RepositoryResult<HiveSettingsModel>> {
        self.inner.load()
    }

    fn save(&self, settings: HiveSettingsModel) -> BoxFuture<'static, RepositoryResult<()>> {
        self.inner.save(settings)
    }
}
