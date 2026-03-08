pub mod conversation;
pub mod conversations_store;
pub mod error_notifier;
pub mod error_store;
pub mod execution_approval_store;
pub mod stream_manager;
pub mod token_usage;
pub mod write_approval_store;

pub use conversation::{Conversation, MessageFeedback};
// Pre-built API: re-exported for external consumers (not yet used via this path)
#[allow(unused_imports)]
pub use conversation::RegenerationRecord;
pub use conversations_store::ConversationsStore;
pub use error_notifier::{ErrorNotifier, ErrorNotifierEvent, GlobalErrorNotifier};
pub use error_store::ErrorStore;
pub use execution_approval_store::ExecutionApprovalStore;
pub use stream_manager::{GlobalStreamManager, StreamManager, StreamManagerEvent, StreamStatus};
pub use write_approval_store::WriteApprovalStore;
