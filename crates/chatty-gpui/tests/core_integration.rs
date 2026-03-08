//! Integration tests verifying chatty-gpui can use chatty-core types correctly.
//!
//! These tests exercise chatty-core's public API from within the chatty-gpui
//! crate context, where the `gpui-globals` feature is always enabled. The goal
//! is to verify that:
//!
//! 1. Core types remain fully functional when compiled with `gpui-globals`.
//! 2. The `impl Global for T` additions don't break normal type usage.
//! 3. Cross-crate type interactions (e.g. settings models used by GPUI controllers)
//!    work correctly.
//!
//! Note: Tests that require a live GPUI `Application` or display connection are
//! not included here — those require a windowed environment not available in CI.

use chatty_core::models::ConversationsStore;
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::{ProviderConfig, ProviderType};
use chatty_core::settings::models::{GeneralSettingsModel, ModelsModel, ProviderModel};
use chatty_core::token_budget::{ContextStatus, TokenBudgetSnapshot};

// ── Core types usable with gpui-globals feature enabled ───────────────────────

/// Verify ConversationsStore is fully functional when compiled with gpui-globals.
/// In chatty-gpui, this type implements gpui::Global so it can be stored via
/// cx.set_global() — but it must remain usable as a plain Rust struct.
#[test]
fn conversations_store_functional_with_gpui_globals() {
    let mut store = ConversationsStore::new();
    assert_eq!(store.count(), 0);

    store.upsert_metadata("conv-a", "First chat", 0.002, 100);
    store.upsert_metadata("conv-b", "Second chat", 0.004, 200);

    assert_eq!(store.count(), 2);
    let list = store.list_recent_metadata(10);
    // Most recent first: conv-b has updated_at=200 > conv-a's 100
    assert_eq!(list[0].0, "conv-b");
    assert_eq!(list[1].0, "conv-a");

    store.set_active_by_id("conv-b".to_string());
    assert_eq!(store.active_id().unwrap(), "conv-b");

    assert!(store.delete_conversation("conv-b"));
    assert_eq!(store.count(), 1);
}

/// Verify ModelsModel is functional with gpui-globals.
/// In chatty-gpui, ModelsModel implements gpui::Global; confirming it can be
/// created and queried without a GPUI context ensures the Global impl is additive.
#[test]
fn models_model_functional_with_gpui_globals() {
    let mut models = ModelsModel::new();
    assert!(models.models().is_empty());

    let openai_model = ModelConfig::new(
        "gpt-4o".to_string(),
        "GPT-4o".to_string(),
        ProviderType::OpenAI,
        "gpt-4o".to_string(),
    );
    models.add_model(openai_model);
    assert_eq!(models.models().len(), 1);

    let found = models.get_model("gpt-4o");
    assert!(found.is_some());
    assert_eq!(found.unwrap().name, "GPT-4o");

    assert!(models.delete_model("gpt-4o"));
    assert!(models.models().is_empty());
}

/// Verify ProviderModel is functional with gpui-globals.
#[test]
fn provider_model_functional_with_gpui_globals() {
    let mut providers = ProviderModel::new();
    assert!(providers.providers().is_empty());

    let anthropic = ProviderConfig::new("Anthropic".to_string(), ProviderType::Anthropic)
        .with_api_key("sk-ant-key".to_string());
    let ollama = ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama);
    providers.add_provider(anthropic);
    providers.add_provider(ollama);

    assert_eq!(providers.providers().len(), 2);

    // Both Anthropic (has key) and Ollama (no key required) should be configured
    let configured: Vec<_> = providers.configured_providers().collect();
    assert_eq!(configured.len(), 2);
}

/// Verify GeneralSettingsModel default and serialization with gpui-globals.
#[test]
fn general_settings_model_functional_with_gpui_globals() {
    let mut settings = GeneralSettingsModel::default();
    assert!((settings.font_size - 14.0).abs() < f32::EPSILON);

    settings.font_size = 18.0;
    settings.theme_name = Some("gruvbox".to_string());

    // JSON serialization must still work (settings are loaded from disk)
    let json = serde_json::to_string(&settings).expect("serialization failed");
    let restored: GeneralSettingsModel =
        serde_json::from_str(&json).expect("deserialization failed");
    assert!((restored.font_size - 18.0).abs() < f32::EPSILON);
    assert_eq!(restored.theme_name.as_deref(), Some("gruvbox"));
}

// ── Cross-crate type interaction (settings + conversations used together) ──────

/// Simulate the startup flow where chatty-gpui loads models, providers, and
/// conversations and wires them together. This mirrors app_controller::new().
#[test]
fn startup_flow_models_providers_conversations_interact() {
    // 1. Initialize all global models (would be cx.set_global() calls in GPUI)
    let mut models_model = ModelsModel::new();
    let mut provider_model = ProviderModel::new();
    let mut conversations_store = ConversationsStore::new();

    // 2. Load providers from disk (simulated)
    let anthropic = ProviderConfig::new("Anthropic".to_string(), ProviderType::Anthropic)
        .with_api_key("sk-ant-api03-abc".to_string());
    provider_model.add_provider(anthropic);

    // 3. Create a model using provider defaults (models_controller path)
    let (img, pdf) = ProviderType::Anthropic.default_capabilities();
    let mut model = ModelConfig::new(
        "claude-sonnet".to_string(),
        "Claude Sonnet".to_string(),
        ProviderType::Anthropic,
        "claude-sonnet-4-20250514".to_string(),
    );
    model.supports_images = img;
    model.supports_pdf = pdf;
    models_model.add_model(model);

    // 4. Load conversation metadata from SQLite (simulated)
    conversations_store.upsert_metadata("conv-1", "Project planning", 0.01, 1_700_000_000);
    conversations_store.set_active_by_id("conv-1".to_string());

    // 5. Verify the state mirrors what the app would have at runtime
    assert_eq!(provider_model.configured_providers().count(), 1);
    assert_eq!(models_model.models().len(), 1);
    assert_eq!(conversations_store.count(), 1);
    assert_eq!(conversations_store.active_id().unwrap(), "conv-1");

    // 6. Model capability check (done before attaching files in chat_input)
    let model = models_model.get_model("claude-sonnet").unwrap();
    assert!(
        model.supports_images,
        "Should support images before sending"
    );
    assert!(model.supports_pdf, "Should support PDFs before sending");
}

// ── Token budget types used by chatty-gpui's token_budget_manager ─────────────

/// TokenBudgetSnapshot is created in chatty-core's token_budget module and
/// consumed by chatty-gpui's UI manager. Verify it's usable across the crate boundary.
#[test]
fn token_budget_snapshot_usable_from_gpui_crate() {
    let snap = TokenBudgetSnapshot {
        computed_at: std::time::Instant::now(),
        model_context_limit: 200_000, // Claude 3 limit
        response_reserve: 8_192,
        preamble_tokens: 500,
        tool_definitions_tokens: 2_000,
        conversation_history_tokens: 15_000,
        latest_user_message_tokens: 200,
        actual_input_tokens: None,
        actual_output_tokens: None,
        conversation_id: "conv-1".to_string(),
    };

    // Verify all calculated properties work
    let effective = snap.effective_budget();
    assert_eq!(effective, 200_000 - 8_192);
    assert_eq!(snap.estimated_total(), 500 + 2_000 + 15_000 + 200);
    assert!(snap.remaining() > 0);
    assert_eq!(snap.status(), ContextStatus::Normal);
    assert!(!snap.has_actuals());
    assert!(snap.estimation_delta().is_none());

    // component_fractions (used by GPUI stacked bar renderer)
    let fracs = snap.component_fractions();
    assert!(fracs.preamble >= 0.0 && fracs.preamble <= 1.0);
    assert!(fracs.tools >= 0.0 && fracs.tools <= 1.0);
    assert!(fracs.history >= 0.0 && fracs.history <= 1.0);
    assert!(fracs.user_msg >= 0.0 && fracs.user_msg <= 1.0);
    assert!(fracs.remaining() >= 0.0 && fracs.remaining() <= 1.0);
}

/// Provider display names are used in chatty-gpui's settings views.
#[test]
fn provider_type_display_names_accessible() {
    assert_eq!(ProviderType::OpenAI.display_name(), "OpenAI");
    assert_eq!(ProviderType::Anthropic.display_name(), "Anthropic");
    assert_eq!(ProviderType::Gemini.display_name(), "Google Gemini");
    assert_eq!(ProviderType::Mistral.display_name(), "Mistral");
    assert_eq!(ProviderType::Ollama.display_name(), "Ollama");
    assert_eq!(ProviderType::AzureOpenAI.display_name(), "Azure OpenAI");
}
