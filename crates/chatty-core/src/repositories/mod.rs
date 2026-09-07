pub mod conversation_repository;
pub mod conversation_sqlite_repository;
pub mod error;
/// Conformance suite for the `RepositoryRegistry` traits plus
/// `ConversationRepository` (AGE-280). Enable `chatty-core/test-support`
/// from a dev-dependency to run it against an out-of-tree implementation.
#[cfg(any(test, feature = "test-support"))]
pub mod store_conformance;

pub use conversation_repository::{ConversationData, ConversationMetadata, ConversationRepository};
pub use conversation_sqlite_repository::ConversationSqliteRepository;
