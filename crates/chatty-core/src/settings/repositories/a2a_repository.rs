use std::future::Future;
use std::pin::Pin;

use super::provider_repository::RepositoryResult;
use crate::settings::models::a2a_store::A2aAgentConfig;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait A2aRepository: Send + Sync + 'static {
    /// Load all A2A agent configurations from storage.
    fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<A2aAgentConfig>>>;

    /// Save all A2A agent configurations to storage.
    fn save_all(
        &self,
        agents: Vec<A2aAgentConfig>,
    ) -> BoxFuture<'static, RepositoryResult<()>>;
}
