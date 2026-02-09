use std::future::Future;
use std::pin::Pin;

use super::provider_repository::RepositoryResult;
use crate::settings::models::mcp_store::McpServerConfig;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait McpRepository: Send + Sync + 'static {
    /// Load all MCP server configurations from storage
    fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<McpServerConfig>>>;

    /// Save all MCP server configurations to storage
    fn save_all(&self, servers: Vec<McpServerConfig>) -> BoxFuture<'static, RepositoryResult<()>>;
}
