/// Shared test helpers for MCP tool unit tests.
///
/// Provides `MockMcpRepository` — a fully in-memory implementation of
/// [`McpRepository`] used by `add_mcp_tool`, `delete_mcp_tool`, and
/// `edit_mcp_tool` test suites.
use crate::settings::models::mcp_store::McpServerConfig;
use crate::settings::repositories::McpRepository;
use crate::settings::repositories::mcp_repository::BoxFuture;
use crate::settings::repositories::provider_repository::{RepositoryError, RepositoryResult};
use std::sync::Mutex;

/// In-memory mock of [`McpRepository`] for unit tests.
pub struct MockMcpRepository {
    pub servers: Mutex<Vec<McpServerConfig>>,
    /// If set, `load_all` returns this error.
    pub load_error: Mutex<Option<String>>,
    /// If set, `save_all` returns this error.
    pub save_error: Mutex<Option<String>>,
    /// Captures the last argument passed to `save_all`.
    pub last_saved: Mutex<Option<Vec<McpServerConfig>>>,
}

impl MockMcpRepository {
    pub fn new() -> Self {
        Self {
            servers: Mutex::new(Vec::new()),
            load_error: Mutex::new(None),
            save_error: Mutex::new(None),
            last_saved: Mutex::new(None),
        }
    }

    pub fn with_servers(servers: Vec<McpServerConfig>) -> Self {
        Self {
            servers: Mutex::new(servers),
            load_error: Mutex::new(None),
            save_error: Mutex::new(None),
            last_saved: Mutex::new(None),
        }
    }

    pub fn with_load_error(error: &str) -> Self {
        Self {
            servers: Mutex::new(Vec::new()),
            load_error: Mutex::new(Some(error.to_string())),
            save_error: Mutex::new(None),
            last_saved: Mutex::new(None),
        }
    }

    pub fn with_save_error(servers: Vec<McpServerConfig>, error: &str) -> Self {
        Self {
            servers: Mutex::new(servers),
            load_error: Mutex::new(None),
            save_error: Mutex::new(Some(error.to_string())),
            last_saved: Mutex::new(None),
        }
    }

    pub fn get_last_saved(&self) -> Option<Vec<McpServerConfig>> {
        self.last_saved.lock().unwrap().clone()
    }
}

impl McpRepository for MockMcpRepository {
    fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<McpServerConfig>>> {
        let servers = self.servers.lock().unwrap().clone();
        let error = self.load_error.lock().unwrap().clone();
        Box::pin(async move {
            if let Some(err) = error {
                Err(RepositoryError::IoError(err))
            } else {
                Ok(servers)
            }
        })
    }

    fn save_all(&self, servers: Vec<McpServerConfig>) -> BoxFuture<'static, RepositoryResult<()>> {
        let error = self.save_error.lock().unwrap().clone();
        *self.last_saved.lock().unwrap() = Some(servers);
        Box::pin(async move {
            if let Some(err) = error {
                Err(RepositoryError::IoError(err))
            } else {
                Ok(())
            }
        })
    }
}
