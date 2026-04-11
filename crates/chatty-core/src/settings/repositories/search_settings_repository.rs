use super::provider_repository::{BoxFuture, RepositoryResult};
use crate::settings::models::search_settings::SearchSettingsModel;

pub trait SearchSettingsRepository: Send + Sync + 'static {
    /// Load search settings from storage
    fn load(&self) -> BoxFuture<'static, RepositoryResult<SearchSettingsModel>>;

    /// Save search settings to storage
    fn save(&self, settings: SearchSettingsModel) -> BoxFuture<'static, RepositoryResult<()>>;
}
