//! Implements `gpui::Global` for chatty-core types.
//!
//! This module is only compiled when the `gpui-globals` feature is enabled.
//! It allows core types to be used with GPUI's `cx.set_global()` / `cx.global()`.

use gpui::Global;

// ── Settings models ──────────────────────────────────────────────────────────
impl Global for crate::settings::models::GeneralSettingsModel {}
impl Global for crate::settings::models::ModelsModel {}
impl Global for crate::settings::models::ProviderModel {}
impl Global for crate::settings::models::McpServersModel {}
impl Global for crate::settings::models::A2aAgentsModel {}
impl Global for crate::settings::models::ExecutionSettingsModel {}
impl Global for crate::settings::models::TrainingSettingsModel {}
impl Global for crate::settings::models::SearchSettingsModel {}
impl Global for crate::settings::models::TokenTrackingSettings {}
impl Global for crate::settings::models::UserSecretsModel {}
impl Global for crate::settings::models::ModuleSettingsModel {}

// ── Chatty models ────────────────────────────────────────────────────────────
impl Global for crate::models::ConversationsStore {}
impl Global for crate::models::ErrorStore {}
impl Global for crate::models::ExecutionApprovalStore {}
impl Global for crate::models::WriteApprovalStore {}

// ── Services ─────────────────────────────────────────────────────────────────
impl Global for crate::services::MathRendererService {}
impl Global for crate::services::McpService {}
impl Global for crate::services::MermaidRendererService {}

// ── Memory ───────────────────────────────────────────────────────────────────
impl Global for crate::services::MemoryService {}
impl Global for crate::services::EmbeddingService {}
impl Global for crate::services::SkillService {}

// ── Auth ─────────────────────────────────────────────────────────────────────
impl Global for crate::auth::AzureTokenCache {}
