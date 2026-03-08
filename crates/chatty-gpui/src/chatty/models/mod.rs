// Re-export everything from chatty-core models (types + submodules)
pub use chatty_core::models::*;

// Local gpui-specific modules
pub mod error_notifier;
pub mod stream_manager;

pub use error_notifier::{ErrorNotifier, ErrorNotifierEvent, GlobalErrorNotifier};
pub use stream_manager::{GlobalStreamManager, StreamManager, StreamManagerEvent, StreamStatus};
