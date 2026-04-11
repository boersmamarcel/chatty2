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

/// Consolidated registry for all repository singletons.
/// Initialized once at startup via `init_repositories()`.
pub struct RepositoryRegistry {
    pub providers: Arc<dyn settings::repositories::ProviderRepository>,
    pub general_settings: Arc<dyn settings::repositories::GeneralSettingsRepository>,
    pub models: Arc<dyn settings::repositories::ModelsRepository>,
    pub mcp: Arc<dyn settings::repositories::McpRepository>,
    pub a2a: Arc<dyn settings::repositories::A2aRepository>,
    pub execution_settings: Arc<dyn settings::repositories::ExecutionSettingsRepository>,
    pub search_settings: Arc<dyn settings::repositories::SearchSettingsRepository>,
    pub training_settings: Arc<dyn settings::repositories::TrainingSettingsRepository>,
    pub user_secrets: Arc<dyn settings::repositories::UserSecretsRepository>,
    pub module_settings: Arc<dyn settings::repositories::ModuleSettingsRepository>,
    pub hive_settings: Arc<dyn settings::repositories::HiveSettingsRepository>,
    pub extensions: Arc<dyn settings::repositories::ExtensionsRepository>,
}

static REPOSITORY_REGISTRY: OnceLock<RepositoryRegistry> = OnceLock::new();

/// Initialize all repository singletons. Must be called once at startup before
/// any repository is accessed. Returns an error if the config directory cannot
/// be determined (e.g., missing HOME), allowing the host to show a proper error
/// dialog instead of panicking.
pub fn init_repositories() -> anyhow::Result<()> {
    use settings::repositories::*;

    let registry = RepositoryRegistry {
        providers: Arc::new(JsonFileRepository::new()?),
        general_settings: Arc::new(GeneralSettingsJsonRepository::new()?),
        models: Arc::new(JsonModelsRepository::new()?),
        mcp: Arc::new(JsonMcpRepository::new()?),
        a2a: Arc::new(A2aJsonRepository::new()?),
        execution_settings: Arc::new(ExecutionSettingsJsonRepository::new()?),
        search_settings: Arc::new(SearchSettingsJsonRepository::new()?),
        training_settings: Arc::new(TrainingSettingsJsonRepository::new()?),
        user_secrets: Arc::new(UserSecretsJsonRepository::new()?),
        module_settings: Arc::new(ModuleSettingsJsonRepository::new()?),
        hive_settings: Arc::new(HiveSettingsJsonRepository::new()?),
        extensions: Arc::new(ExtensionsJsonRepository::new()?),
    };
    REPOSITORY_REGISTRY.set(registry).ok();

    Ok(())
}

fn registry() -> &'static RepositoryRegistry {
    REPOSITORY_REGISTRY
        .get()
        .expect("init_repositories() not called")
}

/// Returns a cloned Arc to the provider repository.
/// Panics if `init_repositories()` was not called — this is a programming error.
pub fn provider_repository() -> Arc<dyn settings::repositories::ProviderRepository> {
    registry().providers.clone()
}

/// Returns a cloned Arc to the general settings repository.
pub fn general_settings_repository() -> Arc<dyn settings::repositories::GeneralSettingsRepository> {
    registry().general_settings.clone()
}

/// Returns a cloned Arc to the models repository.
pub fn models_repository() -> Arc<dyn settings::repositories::ModelsRepository> {
    registry().models.clone()
}

/// Returns a cloned Arc to the MCP repository.
pub fn mcp_repository() -> Arc<dyn settings::repositories::McpRepository> {
    registry().mcp.clone()
}

/// Returns a cloned Arc to the A2A agents repository.
pub fn a2a_repository() -> Arc<dyn settings::repositories::A2aRepository> {
    registry().a2a.clone()
}

/// Returns a cloned Arc to the execution settings repository.
pub fn execution_settings_repository()
-> Arc<dyn settings::repositories::ExecutionSettingsRepository> {
    registry().execution_settings.clone()
}

/// Returns a cloned Arc to the search settings repository.
pub fn search_settings_repository() -> Arc<dyn settings::repositories::SearchSettingsRepository> {
    registry().search_settings.clone()
}

/// Returns a cloned Arc to the training settings repository.
pub fn training_settings_repository() -> Arc<dyn settings::repositories::TrainingSettingsRepository>
{
    registry().training_settings.clone()
}

/// Returns a cloned Arc to the user secrets repository.
pub fn user_secrets_repository() -> Arc<dyn settings::repositories::UserSecretsRepository> {
    registry().user_secrets.clone()
}

/// Returns a cloned Arc to the module settings repository.
pub fn module_settings_repository() -> Arc<dyn settings::repositories::ModuleSettingsRepository> {
    registry().module_settings.clone()
}

/// Returns a cloned Arc to the Hive settings repository.
pub fn hive_settings_repository() -> Arc<dyn settings::repositories::HiveSettingsRepository> {
    registry().hive_settings.clone()
}

/// Returns a cloned Arc to the extensions repository.
pub fn extensions_repository() -> Arc<dyn settings::repositories::ExtensionsRepository> {
    registry().extensions.clone()
}
