// chatty-core: UI-agnostic agent framework, tools, services, and models.
//
// This crate contains no GPUI dependencies and can be used with any UI frontend.

use std::sync::Arc;
use std::sync::OnceLock;

pub mod auth;
pub mod exporters;
pub mod factories;
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

lazy_static::lazy_static! {
    pub static ref PROVIDER_REPOSITORY: Arc<dyn settings::repositories::ProviderRepository> = {
        let repo = settings::repositories::JsonFileRepository::new()
            .expect("Failed to initialize provider repository");
        Arc::new(repo)
    };

    pub static ref GENERAL_SETTINGS_REPOSITORY: Arc<dyn settings::repositories::GeneralSettingsRepository> = {
        let repo = settings::repositories::GeneralSettingsJsonRepository::new()
            .expect("Failed to initialize general settings repository");
        Arc::new(repo)
    };

    pub static ref MODELS_REPOSITORY: Arc<dyn settings::repositories::ModelsRepository> = {
        let repo = settings::repositories::JsonModelsRepository::new()
            .expect("Failed to initialize models repository");
        Arc::new(repo)
    };

    pub static ref MCP_REPOSITORY: Arc<dyn settings::repositories::McpRepository> = {
        let repo = settings::repositories::JsonMcpRepository::new()
            .expect("Failed to initialize MCP repository");
        Arc::new(repo)
    };

    pub static ref EXECUTION_SETTINGS_REPOSITORY: Arc<dyn settings::repositories::ExecutionSettingsRepository> = {
        let repo = settings::repositories::ExecutionSettingsJsonRepository::new()
            .expect("Failed to initialize execution settings repository");
        Arc::new(repo)
    };

    pub static ref TRAINING_SETTINGS_REPOSITORY: Arc<dyn settings::repositories::TrainingSettingsRepository> = {
        let repo = settings::repositories::TrainingSettingsJsonRepository::new()
            .expect("Failed to initialize training settings repository");
        Arc::new(repo)
    };

    pub static ref USER_SECRETS_REPOSITORY: Arc<dyn settings::repositories::UserSecretsRepository> = {
        let repo = settings::repositories::UserSecretsJsonRepository::new()
            .expect("Failed to initialize user secrets repository");
        Arc::new(repo)
    };
}
