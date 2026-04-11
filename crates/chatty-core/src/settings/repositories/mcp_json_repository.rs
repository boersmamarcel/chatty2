use super::generic_json_repository::GenericJsonListRepository;
use super::mcp_repository::McpRepository;
use super::provider_repository::{BoxFuture, RepositoryResult};
use crate::settings::models::mcp_store::McpServerConfig;

pub struct JsonMcpRepository {
    inner: GenericJsonListRepository<McpServerConfig>,
}

impl JsonMcpRepository {
    /// Create repository with XDG-compliant path
    pub fn new() -> RepositoryResult<Self> {
        Ok(Self {
            inner: GenericJsonListRepository::new("mcp_servers.json")?,
        })
    }
}

impl McpRepository for JsonMcpRepository {
    fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<McpServerConfig>>> {
        self.inner.load_all()
    }

    fn save_all(&self, servers: Vec<McpServerConfig>) -> BoxFuture<'static, RepositoryResult<()>> {
        self.inner.save_all(servers)
    }
}
