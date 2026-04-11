use super::provider_repository::{BoxFuture, RepositoryResult};
use crate::settings::models::training_settings::TrainingSettingsModel;

pub trait TrainingSettingsRepository: Send + Sync + 'static {
    /// Load training settings from storage
    fn load(&self) -> BoxFuture<'static, RepositoryResult<TrainingSettingsModel>>;

    /// Save training settings to storage
    fn save(&self, settings: TrainingSettingsModel) -> BoxFuture<'static, RepositoryResult<()>>;
}
