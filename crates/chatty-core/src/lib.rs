// chatty-core: UI-agnostic agent framework, tools, services, and models.
//
// This crate contains no GPUI dependencies and can be used with any UI frontend.
//
// # Singleton Inventory
//
// All process-global singletons are listed here for discoverability.
// They fall into three categories:
//
// ## Service singletons (this file)
// - `MCP_UPDATE_SENDER` — mpsc channel for MCP config updates (OnceLock)
// - `MCP_SERVICE`        — shared McpService instance for tool context (OnceLock)
//
// ## Repository singletons (this file, via RepositoryRegistry)
// - `REGISTRY` — holds all repository Arc<dyn …> instances, initialized by
//   `init_repositories()`. Accessor functions below provide typed access.
//
// ## Domain-local singletons (in their respective modules)
// - `GLOBAL_WRITE_APPROVAL_MODE` — tools/filesystem_write_tool.rs (OnceLock<Mutex>)
// - `AZURE_TOKEN_CACHE`          — factories/agent_factory/mod.rs (LazyLock<Mutex>)
// - `MCP_WRITE_LOCK`             — settings/models/mcp_store.rs (LazyLock<Mutex>)
// - `PATH_AUGMENTED`             — auth/azure_auth.rs (OnceLock<bool>)
// - `OAUTH_CREDENTIAL_REPOSITORY` — settings/repositories/mod.rs (OnceLock)
//
// Design rationale: domain-local singletons stay near their usage to avoid
// coupling unrelated modules through a central registry. Service and repository
// singletons are centralized here because they're cross-cutting concerns
// needed by many modules.

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

/// Returns `true` once `init_repositories()` has completed successfully.
/// Frontends can call this during startup to surface a clear error dialog
/// instead of hitting the panic in `registry()`.
pub fn is_initialized() -> bool {
    REPOSITORY_REGISTRY.get().is_some()
}

fn registry() -> &'static RepositoryRegistry {
    REPOSITORY_REGISTRY.get().expect(
        "BUG: init_repositories() was not called before accessing a repository. \
         This is a programming error in the application startup sequence.",
    )
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

// ── Pre-warming ──────────────────────────────────────────────────────────────

/// Force-initialize expensive lazy statics so the cost is paid in the background
/// rather than causing UI stutter on first use.
///
/// Currently pre-warms:
/// - BPE tokenizer tables (cl100k_base + o200k_base, ~50ms each)
/// - Mermaid/SVG font database (system font scan, ~200-500ms)
pub fn prewarm_statics() {
    // Fire-and-forget: each prewarm runs on its own OS thread so we don't
    // block the GPUI async executor that calls us.
    std::thread::spawn(token_budget::counter::prewarm);
    std::thread::spawn(services::mermaid_renderer_service::prewarm_font_db);
}
