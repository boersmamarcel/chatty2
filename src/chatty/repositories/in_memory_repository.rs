use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::conversation_repository::{BoxFuture, ConversationData, ConversationRepository};
use super::error::{RepositoryError, RepositoryResult};

/// In-memory repository for conversations
/// Useful for testing and development
#[derive(Clone)]
pub struct InMemoryConversationRepository {
    conversations: Arc<Mutex<HashMap<String, ConversationData>>>,
}

impl InMemoryConversationRepository {
    pub fn new() -> Self {
        Self {
            conversations: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Default for InMemoryConversationRepository {
    fn default() -> Self {
        Self::new()
    }
}

impl ConversationRepository for InMemoryConversationRepository {
    fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<ConversationData>>> {
        let conversations = self.conversations.clone();

        Box::pin(async move {
            let store = conversations
                .lock()
                .map_err(|e| RepositoryError::InvalidData {
                    message: format!("Failed to lock conversations: {}", e),
                })?;

            let mut result: Vec<ConversationData> = store.values().cloned().collect();

            // Sort by updated_at descending
            result.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

            Ok(result)
        })
    }

    fn save(&self, id: &str, data: ConversationData) -> BoxFuture<'static, RepositoryResult<()>> {
        let conversations = self.conversations.clone();
        let id = id.to_string();

        Box::pin(async move {
            let mut store = conversations
                .lock()
                .map_err(|e| RepositoryError::InvalidData {
                    message: format!("Failed to lock conversations: {}", e),
                })?;

            store.insert(id, data);

            Ok(())
        })
    }

    fn delete(&self, id: &str) -> BoxFuture<'static, RepositoryResult<()>> {
        let conversations = self.conversations.clone();
        let id = id.to_string();

        Box::pin(async move {
            let mut store = conversations
                .lock()
                .map_err(|e| RepositoryError::InvalidData {
                    message: format!("Failed to lock conversations: {}", e),
                })?;

            store.remove(&id);

            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_save_and_load() {
        let repo = InMemoryConversationRepository::new();

        let data = ConversationData {
            id: "test-1".to_string(),
            title: "Test Conversation".to_string(),
            model_id: "model-1".to_string(),
            message_history: "[]".to_string(),
            system_traces: "[]".to_string(),
            token_usage: "{}".to_string(),
            attachment_paths: "[]".to_string(),
            message_timestamps: "[]".to_string(),
            created_at: 1000,
            updated_at: 1000,
        };

        repo.save("test-1", data.clone()).await.unwrap();

        let loaded = repo.load_all().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "test-1");
        assert_eq!(loaded[0].title, "Test Conversation");
    }

    #[tokio::test]
    async fn test_delete() {
        let repo = InMemoryConversationRepository::new();

        let data = ConversationData {
            id: "test-1".to_string(),
            title: "Test Conversation".to_string(),
            model_id: "model-1".to_string(),
            message_history: "[]".to_string(),
            system_traces: "[]".to_string(),
            token_usage: "{}".to_string(),
            attachment_paths: "[]".to_string(),
            message_timestamps: "[]".to_string(),
            created_at: 1000,
            updated_at: 1000,
        };

        repo.save("test-1", data).await.unwrap();
        repo.delete("test-1").await.unwrap();

        let loaded = repo.load_all().await.unwrap();
        assert_eq!(loaded.len(), 0);
    }

    #[tokio::test]
    async fn test_sorting_by_updated_at() {
        let repo = InMemoryConversationRepository::new();

        let data1 = ConversationData {
            id: "test-1".to_string(),
            title: "Older".to_string(),
            model_id: "model-1".to_string(),
            message_history: "[]".to_string(),
            system_traces: "[]".to_string(),
            token_usage: "{}".to_string(),
            attachment_paths: "[]".to_string(),
            message_timestamps: "[]".to_string(),
            created_at: 1000,
            updated_at: 1000,
        };

        let data2 = ConversationData {
            id: "test-2".to_string(),
            title: "Newer".to_string(),
            model_id: "model-1".to_string(),
            message_history: "[]".to_string(),
            system_traces: "[]".to_string(),
            token_usage: "{}".to_string(),
            attachment_paths: "[]".to_string(),
            message_timestamps: "[]".to_string(),
            created_at: 2000,
            updated_at: 2000,
        };

        repo.save("test-1", data1).await.unwrap();
        repo.save("test-2", data2).await.unwrap();

        let loaded = repo.load_all().await.unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].title, "Newer");
        assert_eq!(loaded[1].title, "Older");
    }
}
