use std::pin::Pin;

use crate::settings::models::module_settings::ModuleSettingsModel;
use crate::settings::repositories::provider_repository::RepositoryResult;

pub type BoxFuture<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

pub trait ModuleSettingsRepository: Send + Sync {
    fn load(&self) -> BoxFuture<'static, RepositoryResult<ModuleSettingsModel>>;
    fn save(&self, settings: ModuleSettingsModel) -> BoxFuture<'static, RepositoryResult<()>>;
}
