use std::pin::Pin;

use crate::settings::models::hive_settings::HiveSettingsModel;
use crate::settings::repositories::provider_repository::RepositoryResult;

pub type BoxFuture<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

pub trait HiveSettingsRepository: Send + Sync {
    fn load(&self) -> BoxFuture<'static, RepositoryResult<HiveSettingsModel>>;
    fn save(&self, settings: HiveSettingsModel) -> BoxFuture<'static, RepositoryResult<()>>;
}
