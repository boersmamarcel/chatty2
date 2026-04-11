use super::provider_repository::{BoxFuture, RepositoryResult};
use crate::settings::models::a2a_store::A2aAgentConfig;

pub trait A2aRepository: Send + Sync + 'static {
    /// Load all A2A agent configurations from storage.
    fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<A2aAgentConfig>>>;

    /// Save all A2A agent configurations to storage.
    fn save_all(&self, agents: Vec<A2aAgentConfig>) -> BoxFuture<'static, RepositoryResult<()>>;
}
