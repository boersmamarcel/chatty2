/// Shared test helpers for MCP and A2A tool unit tests.
///
/// Provides:
/// - `MockMcpRepository` — in-memory [`McpRepository`] for MCP tool tests.
/// - `MockA2aRepository` — in-memory [`A2aRepository`] for A2A tool tests.
use crate::settings::models::a2a_store::A2aAgentConfig;
use crate::settings::models::mcp_store::McpServerConfig;
use crate::settings::repositories::provider_repository::{
    BoxFuture, RepositoryError, RepositoryResult,
};
use crate::settings::repositories::{A2aRepository, McpRepository};
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

/// In-memory mock of [`A2aRepository`] for unit tests.
pub struct MockA2aRepository {
    pub agents: Mutex<Vec<A2aAgentConfig>>,
    /// If set, `load_all` returns this error.
    pub load_error: Mutex<Option<String>>,
}

impl MockA2aRepository {
    pub fn new() -> Self {
        Self {
            agents: Mutex::new(Vec::new()),
            load_error: Mutex::new(None),
        }
    }

    pub fn with_agents(agents: Vec<A2aAgentConfig>) -> Self {
        Self {
            agents: Mutex::new(agents),
            load_error: Mutex::new(None),
        }
    }

    pub fn with_load_error(error: &str) -> Self {
        Self {
            agents: Mutex::new(Vec::new()),
            load_error: Mutex::new(Some(error.to_string())),
        }
    }
}

impl A2aRepository for MockA2aRepository {
    fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<A2aAgentConfig>>> {
        let agents = self.agents.lock().unwrap().clone();
        let error = self.load_error.lock().unwrap().clone();
        Box::pin(async move {
            if let Some(err) = error {
                Err(RepositoryError::IoError(err))
            } else {
                Ok(agents)
            }
        })
    }

    fn save_all(&self, _agents: Vec<A2aAgentConfig>) -> BoxFuture<'static, RepositoryResult<()>> {
        Box::pin(async move { Ok(()) })
    }
}
