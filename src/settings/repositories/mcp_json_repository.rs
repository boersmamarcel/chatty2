use std::path::PathBuf;

use super::mcp_repository::{BoxFuture, McpRepository};
use super::provider_repository::{RepositoryError, RepositoryResult};
use crate::settings::models::mcp_store::McpServerConfig;

pub struct JsonMcpRepository {
    file_path: PathBuf,
}

impl JsonMcpRepository {
    /// Create repository with XDG-compliant path
    pub fn new() -> RepositoryResult<Self> {
        let config_dir = dirs::config_dir().ok_or_else(|| {
            RepositoryError::PathError("Cannot determine config directory".into())
        })?;

        let app_dir = config_dir.join("chatty");
        let file_path = app_dir.join("mcp_servers.json");

        Ok(Self { file_path })
    }
}

impl McpRepository for JsonMcpRepository {
    fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<McpServerConfig>>> {
        let path = self.file_path.clone();

        Box::pin(async move {
            // Check if file exists
            if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
                return Ok(Vec::new());
            }

            // Read file
            let contents = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            // Parse JSON
            let configs: Vec<McpServerConfig> = serde_json::from_str(&contents)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            Ok(configs)
        })
    }

    fn save_all(&self, servers: Vec<McpServerConfig>) -> BoxFuture<'static, RepositoryResult<()>> {
        let path = self.file_path.clone();

        Box::pin(async move {
            // Serialize to JSON
            let json = serde_json::to_string_pretty(&servers)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            // Create directory if needed
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| RepositoryError::IoError(e.to_string()))?;
            }

            // Write atomically using temp file + rename
            let temp_path = path.with_extension("json.tmp");
            tokio::fs::write(&temp_path, &json)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            tokio::fs::rename(&temp_path, &path)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            Ok(())
        })
    }
}
