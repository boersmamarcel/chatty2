// chatty-core: UI-agnostic agent framework, tools, services, and models.
//
// This crate contains no GPUI dependencies and can be used with any UI frontend.

use std::sync::Arc;
use std::sync::OnceLock;

pub mod auth;
pub mod exporters;
pub mod factories;
pub use hive_client as hive;
pub mod install;
pub mod migration;
pub mod models;
pub mod repositories;
pub mod sandbox;
pub mod services;
pub mod settings;
pub mod token_budget;
pub mod tools;

// ── GPUI integration (optional feature) ──────────────────────────────────────
#[cfg(feature = "gpui-globals")]
mod gpui_globals;

// ── Global singletons ────────────────────────────────────────────────────────
// These are initialized once at startup by the host application (GPUI, TUI, etc.)

/// Sender half of the MCP update channel. Initialized once at startup.
/// AddMcpTool sends the updated server list here after a successful save.
pub static MCP_UPDATE_SENDER: OnceLock<
    tokio::sync::mpsc::Sender<Vec<settings::models::mcp_store::McpServerConfig>>,
> = OnceLock::new();

/// McpService instance accessible from tool context (no UI framework available there).
pub static MCP_SERVICE: OnceLock<services::McpService> = OnceLock::new();

// ── Repository singletons ────────────────────────────────────────────────────
// Initialized via `init_repositories()` at startup. Access via accessor functions below.

static PROVIDER_REPOSITORY: OnceLock<Arc<dyn settings::repositories::ProviderRepository>> =
    OnceLock::new();
static GENERAL_SETTINGS_REPOSITORY: OnceLock<
    Arc<dyn settings::repositories::GeneralSettingsRepository>,
> = OnceLock::new();
static MODELS_REPOSITORY: OnceLock<Arc<dyn settings::repositories::ModelsRepository>> =
    OnceLock::new();
static MCP_REPOSITORY: OnceLock<Arc<dyn settings::repositories::McpRepository>> = OnceLock::new();
static A2A_REPOSITORY: OnceLock<Arc<dyn settings::repositories::A2aRepository>> = OnceLock::new();
static EXECUTION_SETTINGS_REPOSITORY: OnceLock<
    Arc<dyn settings::repositories::ExecutionSettingsRepository>,
> = OnceLock::new();
static SEARCH_SETTINGS_REPOSITORY: OnceLock<
    Arc<dyn settings::repositories::SearchSettingsRepository>,
> = OnceLock::new();
static TRAINING_SETTINGS_REPOSITORY: OnceLock<
    Arc<dyn settings::repositories::TrainingSettingsRepository>,
> = OnceLock::new();
static USER_SECRETS_REPOSITORY: OnceLock<Arc<dyn settings::repositories::UserSecretsRepository>> =
    OnceLock::new();
static MODULE_SETTINGS_REPOSITORY: OnceLock<
    Arc<dyn settings::repositories::ModuleSettingsRepository>,
> = OnceLock::new();
static HIVE_SETTINGS_REPOSITORY: OnceLock<Arc<dyn settings::repositories::HiveSettingsRepository>> =
    OnceLock::new();
static EXTENSIONS_REPOSITORY: OnceLock<Arc<dyn settings::repositories::ExtensionsRepository>> =
    OnceLock::new();

/// Initialize all repository singletons. Must be called once at startup before
/// any repository is accessed. Returns an error if the config directory cannot
/// be determined (e.g., missing HOME), allowing the host to show a proper error
/// dialog instead of panicking.
pub fn init_repositories() -> anyhow::Result<()> {
    use settings::repositories::*;

    PROVIDER_REPOSITORY
        .set(Arc::new(JsonFileRepository::new()?))
        .ok();
    GENERAL_SETTINGS_REPOSITORY
        .set(Arc::new(GeneralSettingsJsonRepository::new()?))
        .ok();
    MODELS_REPOSITORY
        .set(Arc::new(JsonModelsRepository::new()?))
        .ok();
    MCP_REPOSITORY.set(Arc::new(JsonMcpRepository::new()?)).ok();
    A2A_REPOSITORY.set(Arc::new(A2aJsonRepository::new()?)).ok();
    EXECUTION_SETTINGS_REPOSITORY
        .set(Arc::new(ExecutionSettingsJsonRepository::new()?))
        .ok();
    SEARCH_SETTINGS_REPOSITORY
        .set(Arc::new(SearchSettingsJsonRepository::new()?))
        .ok();
    TRAINING_SETTINGS_REPOSITORY
        .set(Arc::new(TrainingSettingsJsonRepository::new()?))
        .ok();
    USER_SECRETS_REPOSITORY
        .set(Arc::new(UserSecretsJsonRepository::new()?))
        .ok();
    MODULE_SETTINGS_REPOSITORY
        .set(Arc::new(ModuleSettingsJsonRepository::new()?))
        .ok();
    HIVE_SETTINGS_REPOSITORY
        .set(Arc::new(HiveSettingsJsonRepository::new()?))
        .ok();
    EXTENSIONS_REPOSITORY
        .set(Arc::new(ExtensionsJsonRepository::new()?))
        .ok();

    Ok(())
}

/// Returns a cloned Arc to the provider repository.
/// Panics if `init_repositories()` was not called — this is a programming error.
pub fn provider_repository() -> Arc<dyn settings::repositories::ProviderRepository> {
    PROVIDER_REPOSITORY
        .get()
        .expect("init_repositories() not called")
        .clone()
}

/// Returns a cloned Arc to the general settings repository.
pub fn general_settings_repository() -> Arc<dyn settings::repositories::GeneralSettingsRepository> {
    GENERAL_SETTINGS_REPOSITORY
        .get()
        .expect("init_repositories() not called")
        .clone()
}

/// Returns a cloned Arc to the models repository.
pub fn models_repository() -> Arc<dyn settings::repositories::ModelsRepository> {
    MODELS_REPOSITORY
        .get()
        .expect("init_repositories() not called")
        .clone()
}

/// Returns a cloned Arc to the MCP repository.
pub fn mcp_repository() -> Arc<dyn settings::repositories::McpRepository> {
    MCP_REPOSITORY
        .get()
        .expect("init_repositories() not called")
        .clone()
}

/// Returns a cloned Arc to the A2A agents repository.
pub fn a2a_repository() -> Arc<dyn settings::repositories::A2aRepository> {
    A2A_REPOSITORY
        .get()
        .expect("init_repositories() not called")
        .clone()
}

/// Returns a cloned Arc to the execution settings repository.
pub fn execution_settings_repository()
-> Arc<dyn settings::repositories::ExecutionSettingsRepository> {
    EXECUTION_SETTINGS_REPOSITORY
        .get()
        .expect("init_repositories() not called")
        .clone()
}

/// Returns a cloned Arc to the search settings repository.
pub fn search_settings_repository() -> Arc<dyn settings::repositories::SearchSettingsRepository> {
    SEARCH_SETTINGS_REPOSITORY
        .get()
        .expect("init_repositories() not called")
        .clone()
}

/// Returns a cloned Arc to the training settings repository.
pub fn training_settings_repository() -> Arc<dyn settings::repositories::TrainingSettingsRepository>
{
    TRAINING_SETTINGS_REPOSITORY
        .get()
        .expect("init_repositories() not called")
        .clone()
}

/// Returns a cloned Arc to the user secrets repository.
pub fn user_secrets_repository() -> Arc<dyn settings::repositories::UserSecretsRepository> {
    USER_SECRETS_REPOSITORY
        .get()
        .expect("init_repositories() not called")
        .clone()
}

/// Returns a cloned Arc to the module settings repository.
pub fn module_settings_repository() -> Arc<dyn settings::repositories::ModuleSettingsRepository> {
    MODULE_SETTINGS_REPOSITORY
        .get()
        .expect("init_repositories() not called")
        .clone()
}

/// Returns a cloned Arc to the Hive settings repository.
pub fn hive_settings_repository() -> Arc<dyn settings::repositories::HiveSettingsRepository> {
    HIVE_SETTINGS_REPOSITORY
        .get()
        .expect("init_repositories() not called")
        .clone()
}

/// Returns a cloned Arc to the extensions repository.
pub fn extensions_repository() -> Arc<dyn settings::repositories::ExtensionsRepository> {
    EXTENSIONS_REPOSITORY
        .get()
        .expect("init_repositories() not called")
        .clone()
}
