use std::future::Future;
use std::pin::Pin;

use super::provider_repository::RepositoryResult;
use crate::settings::models::execution_settings::ExecutionSettingsModel;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait ExecutionSettingsRepository: Send + Sync + 'static {
    /// Load execution settings from storage
    fn load(&self) -> BoxFuture<'static, RepositoryResult<ExecutionSettingsModel>>;

    /// Save execution settings to storage
    fn save(
        &self,
        settings: ExecutionSettingsModel,
    ) -> BoxFuture<'static, RepositoryResult<()>>;
}
