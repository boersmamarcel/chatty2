pub mod conversation;
pub mod conversations_store;

pub use conversation::Conversation;
pub use conversations_store::ConversationsModel;

// Re-export StreamChunk from services for backward compatibility
pub use crate::chatty::services::StreamChunk;
