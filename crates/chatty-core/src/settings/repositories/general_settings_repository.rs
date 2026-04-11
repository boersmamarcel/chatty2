use super::provider_repository::{BoxFuture, RepositoryResult};
use crate::settings::models::general_model::GeneralSettingsModel;

pub trait GeneralSettingsRepository: Send + Sync + 'static {
    /// Load general settings from storage
    fn load(&self) -> BoxFuture<'static, RepositoryResult<GeneralSettingsModel>>;

    /// Save general settings to storage
    fn save(&self, settings: GeneralSettingsModel) -> BoxFuture<'static, RepositoryResult<()>>;
}
