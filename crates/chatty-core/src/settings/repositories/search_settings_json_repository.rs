use super::generic_json_repository::GenericJsonRepository;
use super::provider_repository::{BoxFuture, RepositoryResult};
use super::search_settings_repository::SearchSettingsRepository;
use crate::settings::models::search_settings::SearchSettingsModel;

pub struct SearchSettingsJsonRepository {
    inner: GenericJsonRepository<SearchSettingsModel>,
}

impl SearchSettingsJsonRepository {
    /// Create repository with XDG-compliant path
    pub fn new() -> RepositoryResult<Self> {
        Ok(Self {
            inner: GenericJsonRepository::new("search_settings.json")?,
        })
    }
}

impl SearchSettingsRepository for SearchSettingsJsonRepository {
    fn load(&self) -> BoxFuture<'static, RepositoryResult<SearchSettingsModel>> {
        self.inner.load()
    }

    fn save(&self, settings: SearchSettingsModel) -> BoxFuture<'static, RepositoryResult<()>> {
        self.inner.save(settings)
    }
}
