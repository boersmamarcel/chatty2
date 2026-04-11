use super::a2a_repository::A2aRepository;
use super::generic_json_repository::GenericJsonListRepository;
use super::provider_repository::{BoxFuture, RepositoryResult};
use crate::settings::models::a2a_store::A2aAgentConfig;

pub struct A2aJsonRepository {
    inner: GenericJsonListRepository<A2aAgentConfig>,
}

impl A2aJsonRepository {
    /// Create repository using the XDG-compliant config directory.
    pub fn new() -> RepositoryResult<Self> {
        Ok(Self {
            inner: GenericJsonListRepository::new("a2a_agents.json")?,
        })
    }
}

impl A2aRepository for A2aJsonRepository {
    fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<A2aAgentConfig>>> {
        self.inner.load_all()
    }

    fn save_all(&self, agents: Vec<A2aAgentConfig>) -> BoxFuture<'static, RepositoryResult<()>> {
        self.inner.save_all(agents)
    }
}
