use std::future::Future;
use std::pin::Pin;

use super::provider_repository::RepositoryResult;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Repository for per-server OAuth credentials (tokens, client IDs, scopes).
///
/// Credentials are stored per MCP server name as opaque JSON values,
/// keeping this trait free of rmcp-specific types.
pub trait OAuthCredentialRepository: Send + Sync + 'static {
    /// Load stored OAuth credentials for a server.
    fn load(&self, server_name: &str)
        -> BoxFuture<'static, RepositoryResult<Option<serde_json::Value>>>;

    /// Persist OAuth credentials for a server.
    fn save(
        &self,
        server_name: &str,
        credentials: serde_json::Value,
    ) -> BoxFuture<'static, RepositoryResult<()>>;

    /// Remove stored credentials for a server.
    fn clear(&self, server_name: &str) -> BoxFuture<'static, RepositoryResult<()>>;

    /// Check if credentials exist for a server (without loading them).
    fn has_credentials(&self, server_name: &str) -> bool;
}
