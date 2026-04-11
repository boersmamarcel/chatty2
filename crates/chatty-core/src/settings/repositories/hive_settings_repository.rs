use crate::settings::models::hive_settings::HiveSettingsModel;
use crate::settings::repositories::provider_repository::{BoxFuture, RepositoryResult};

pub trait HiveSettingsRepository: Send + Sync {
    fn load(&self) -> BoxFuture<'static, RepositoryResult<HiveSettingsModel>>;
    fn save(&self, settings: HiveSettingsModel) -> BoxFuture<'static, RepositoryResult<()>>;
}
