use std::future::Future;
use std::pin::Pin;

use super::provider_repository::RepositoryResult;
use crate::settings::models::search_settings::SearchSettingsModel;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait SearchSettingsRepository: Send + Sync + 'static {
    /// Load search settings from storage
    fn load(&self) -> BoxFuture<'static, RepositoryResult<SearchSettingsModel>>;

    /// Save search settings to storage
    fn save(&self, settings: SearchSettingsModel) -> BoxFuture<'static, RepositoryResult<()>>;
}
