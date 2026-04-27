use crate::settings::models::module_settings::ModuleSettingsModel;
use crate::settings::repositories::provider_repository::{BoxFuture, RepositoryResult};

pub trait ModuleSettingsRepository: Send + Sync {
    fn load(&self) -> BoxFuture<'static, RepositoryResult<ModuleSettingsModel>>;
    fn save(&self, settings: ModuleSettingsModel) -> BoxFuture<'static, RepositoryResult<()>>;
}
