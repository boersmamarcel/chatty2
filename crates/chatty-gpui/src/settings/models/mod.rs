// Re-export everything from chatty-core settings models
pub use chatty_core::settings::models::*;

// Re-export submodules for path-based access
pub use chatty_core::settings::models::{
    execution_settings, extensions_store, general_model, hive_settings, mcp_store, models_store,
    module_settings, providers_store, search_settings, token_tracking_settings, training_settings,
    user_secrets_store,
};

// Local gpui-specific modules
pub mod agent_config_notifier;
pub mod discovered_modules;
pub mod models_notifier;

pub use agent_config_notifier::{AgentConfigEvent, AgentConfigNotifier, GlobalAgentConfigNotifier};
pub use discovered_modules::{DiscoveredModuleEntry, DiscoveredModulesModel, ModuleLoadStatus};
pub use models_notifier::{GlobalModelsNotifier, ModelsNotifier, ModelsNotifierEvent};
