use std::future::Future;
use std::pin::Pin;

use super::provider_repository::RepositoryResult;
use crate::settings::models::training_settings::TrainingSettingsModel;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[allow(dead_code)]
pub trait TrainingSettingsRepository: Send + Sync + 'static {
    /// Load training settings from storage
    fn load(&self) -> BoxFuture<'static, RepositoryResult<TrainingSettingsModel>>;

    /// Save training settings to storage
    fn save(&self, settings: TrainingSettingsModel) -> BoxFuture<'static, RepositoryResult<()>>;
}
