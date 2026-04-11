use super::provider_repository::{BoxFuture, RepositoryResult};
use crate::settings::models::execution_settings::ExecutionSettingsModel;

pub trait ExecutionSettingsRepository: Send + Sync + 'static {
    /// Load execution settings from storage
    fn load(&self) -> BoxFuture<'static, RepositoryResult<ExecutionSettingsModel>>;

    /// Save execution settings to storage
    fn save(&self, settings: ExecutionSettingsModel) -> BoxFuture<'static, RepositoryResult<()>>;
}
