use std::path::PathBuf;

use super::conversation_repository::{BoxFuture, ConversationData, ConversationRepository};
use super::error::{RepositoryError, RepositoryResult};

/// JSON file-based repository for conversations
/// Stores each conversation as a separate file in ~/.config/chatty/conversations/
pub struct ConversationJsonRepository {
    conversations_dir: PathBuf,
}

impl ConversationJsonRepository {
    pub fn new() -> RepositoryResult<Self> {
        let config_dir = dirs::config_dir()
            .ok_or_else(|| RepositoryError::InitializationError {
                message: "Could not determine config directory".to_string(),
            })?
            .join("chatty")
            .join("conversations");

        Ok(Self {
            conversations_dir: config_dir,
        })
    }

    fn get_conversation_path(&self, id: &str) -> PathBuf {
        self.conversations_dir.join(format!("{}.json", id))
    }
}

impl Default for ConversationJsonRepository {
    fn default() -> Self {
        Self::new().expect("Failed to create ConversationJsonRepository")
    }
}

impl ConversationRepository for ConversationJsonRepository {
    fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<ConversationData>>> {
        let conversations_dir = self.conversations_dir.clone();

        Box::pin(async move {
            // Ensure directory exists
            smol::unblock(move || {
                std::fs::create_dir_all(&conversations_dir)?;

                let mut conversations = Vec::new();

                // Read all .json files in the directory
                for entry in std::fs::read_dir(&conversations_dir)? {
                    let entry = entry?;
                    let path = entry.path();

                    if path.extension().and_then(|s| s.to_str()) == Some("json") {
                        let content = std::fs::read_to_string(&path)?;
                        let data: ConversationData = serde_json::from_str(&content)?;
                        conversations.push(data);
                    }
                }

                // Sort by updated_at descending
                conversations.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

                Ok(conversations)
            })
            .await
        })
    }

    fn save(&self, id: &str, data: ConversationData) -> BoxFuture<'static, RepositoryResult<()>> {
        let path = self.get_conversation_path(id);
        let conversations_dir = self.conversations_dir.clone();

        Box::pin(async move {
            smol::unblock(move || {
                // Ensure directory exists
                std::fs::create_dir_all(&conversations_dir)?;

                // Serialize to JSON
                let json = serde_json::to_string_pretty(&data)?;

                // Write to file atomically (write to temp, then rename)
                let temp_path = path.with_extension("json.tmp");
                std::fs::write(&temp_path, json)?;
                std::fs::rename(&temp_path, &path)?;

                Ok(())
            })
            .await
        })
    }

    fn delete(&self, id: &str) -> BoxFuture<'static, RepositoryResult<()>> {
        let path = self.get_conversation_path(id);

        Box::pin(async move {
            smol::unblock(move || {
                if path.exists() {
                    std::fs::remove_file(&path)?;
                }
                Ok(())
            })
            .await
        })
    }
}
