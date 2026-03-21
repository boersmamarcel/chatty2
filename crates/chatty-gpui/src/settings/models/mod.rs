// Re-export everything from chatty-core settings models
pub use chatty_core::settings::models::*;

// Re-export submodules for path-based access
pub use chatty_core::settings::models::{
    browser_credentials_store, execution_settings, general_model, mcp_store, models_store,
    providers_store, search_settings, token_tracking_settings, training_settings,
    user_secrets_store,
};

// Local gpui-specific modules
pub mod agent_config_notifier;
pub mod models_notifier;

pub use agent_config_notifier::{AgentConfigEvent, AgentConfigNotifier, GlobalAgentConfigNotifier};
pub use models_notifier::{GlobalModelsNotifier, ModelsNotifier, ModelsNotifierEvent};
