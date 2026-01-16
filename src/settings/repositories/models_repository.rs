use std::future::Future;
use std::pin::Pin;

use super::provider_repository::{RepositoryError, RepositoryResult};
use crate::settings::models::models_store::ModelConfig;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait ModelsRepository: Send + Sync + 'static {
    /// Load all model configurations from storage
    fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<ModelConfig>>>;

    /// Save all model configurations to storage
    fn save_all(&self, models: Vec<ModelConfig>) -> BoxFuture<'static, RepositoryResult<()>>;
}
