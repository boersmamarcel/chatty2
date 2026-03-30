use std::path::PathBuf;

use super::a2a_repository::{A2aRepository, BoxFuture};
use super::provider_repository::{RepositoryError, RepositoryResult};
use crate::settings::models::a2a_store::A2aAgentConfig;

pub struct A2aJsonRepository {
    file_path: PathBuf,
}

impl A2aJsonRepository {
    /// Create repository using the XDG-compliant config directory.
    pub fn new() -> RepositoryResult<Self> {
        let config_dir = dirs::config_dir().ok_or_else(|| {
            RepositoryError::PathError("Cannot determine config directory".into())
        })?;

        let file_path = config_dir.join("chatty").join("a2a_agents.json");
        Ok(Self { file_path })
    }
}

impl A2aRepository for A2aJsonRepository {
    fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<A2aAgentConfig>>> {
        let path = self.file_path.clone();

        Box::pin(async move {
            if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
                return Ok(Vec::new());
            }

            let contents = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            let agents: Vec<A2aAgentConfig> = serde_json::from_str(&contents)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            Ok(agents)
        })
    }

    fn save_all(&self, agents: Vec<A2aAgentConfig>) -> BoxFuture<'static, RepositoryResult<()>> {
        let path = self.file_path.clone();

        Box::pin(async move {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| RepositoryError::IoError(e.to_string()))?;
            }

            let contents = serde_json::to_string_pretty(&agents)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            tokio::fs::write(&path, contents)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            Ok(())
        })
    }
}
