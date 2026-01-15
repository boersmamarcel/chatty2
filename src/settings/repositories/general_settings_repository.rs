use std::future::Future;
use std::pin::Pin;

use super::provider_repository::RepositoryResult;
use crate::settings::models::general_model::GeneralSettingsModel;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait GeneralSettingsRepository: Send + Sync + 'static {
    /// Load general settings from storage
    fn load(&self) -> BoxFuture<'static, RepositoryResult<GeneralSettingsModel>>;

    /// Save general settings to storage
    fn save(&self, settings: GeneralSettingsModel) -> BoxFuture<'static, RepositoryResult<()>>;
}
