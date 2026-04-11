use super::provider_repository::{BoxFuture, RepositoryResult};
use crate::settings::models::mcp_store::McpServerConfig;

pub trait McpRepository: Send + Sync + 'static {
    /// Load all MCP server configurations from storage
    fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<McpServerConfig>>>;

    /// Save all MCP server configurations to storage
    fn save_all(&self, servers: Vec<McpServerConfig>) -> BoxFuture<'static, RepositoryResult<()>>;
}
