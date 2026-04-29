//! Integration tests for the chatty-core crate.
//!
//! These tests verify the public API surface of chatty-core from an external
//! consumer's perspective (no access to private types or modules). They focus
//! on cross-module interactions that mirror what chatty-gpui's controllers
//! actually perform at runtime.
//!
//! Tests are grouped by the interaction they verify:
//! - Settings models: ModelsModel + ProviderModel + GeneralSettingsModel
//! - Conversations store: ConversationsStore CRUD lifecycle
//! - Provider capabilities: ProviderType defaults propagated to ModelConfig
//! - Serialization: ModelConfig and ProviderConfig roundtrip through JSON
//! - Token budget: TokenBudgetSnapshot status calculations

use chatty_core::models::ConversationsStore;
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::{
    AzureAuthMethod, ProviderConfig, ProviderType,
};
use chatty_core::settings::models::{GeneralSettingsModel, ModelsModel, ProviderModel};
use chatty_core::token_budget::{ContextStatus, TokenBudgetSnapshot};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_snapshot(used_tokens: usize, model_limit: usize, reserve: usize) -> TokenBudgetSnapshot {
    // Distribute `used_tokens` evenly across all four components
    let quarter = used_tokens / 4;
    TokenBudgetSnapshot {
        computed_at: std::time::Instant::now(),
        model_context_limit: model_limit,
        response_reserve: reserve,
        preamble_tokens: quarter,
        tool_definitions_tokens: quarter,
        conversation_history_tokens: quarter,
        latest_user_message_tokens: used_tokens - quarter * 3, // absorb rounding
        actual_input_tokens: None,
        actual_output_tokens: None,
        conversation_id: "test-conv-id".to_string(),
    }
}

// ── Settings models: ModelsModel + ProviderModel used together ────────────────

/// Mirrors the startup flow in chatty-gpui's app_controller: load providers and
/// models, then query which providers are configured before sending a message.
#[test]
fn settings_models_work_together_at_startup() {
    let mut models_model = ModelsModel::new();
    let mut provider_model = ProviderModel::new();

    // Add a provider (simulating loading from disk via repository)
    let provider = ProviderConfig::new("My OpenRouter".to_string(), ProviderType::OpenRouter)
        .with_api_key("sk-or-test-key".to_string());
    provider_model.add_provider(provider);

    // Use provider defaults to create a model (as models_controller::create_model does)
    let provider_type = ProviderType::OpenRouter;
    let (supports_images, supports_pdf) = provider_type.default_capabilities();
    let mut model = ModelConfig::new(
        "model-1".to_string(),
        "Claude Sonnet (via OpenRouter)".to_string(),
        provider_type,
        "anthropic/claude-sonnet-4-20250514".to_string(),
    );
    model.supports_images = supports_images;
    model.supports_pdf = supports_pdf;
    models_model.add_model(model);

    // Verify the OpenRouter provider is configured (has a non-empty API key)
    let configured: Vec<_> = provider_model.configured_providers().collect();
    assert_eq!(
        configured.len(),
        1,
        "Provider with API key should be configured"
    );

    // Verify the model reflects OpenRouter's default capabilities
    let model = models_model.get_model("model-1").unwrap();
    assert!(
        model.supports_images,
        "OpenRouter models should support images"
    );
    assert!(model.supports_pdf, "OpenRouter models should support PDFs");
}

/// Verify ModelsModel CRUD operations across all steps that chatty-gpui performs.
#[test]
fn models_model_full_crud_lifecycle() {
    let mut models = ModelsModel::new();

    // Add models for different providers
    let openrouter_model = ModelConfig::new(
        "id-openrouter".to_string(),
        "Claude Sonnet (via OpenRouter)".to_string(),
        ProviderType::OpenRouter,
        "anthropic/claude-sonnet-4-20250514".to_string(),
    );
    let ollama_model = ModelConfig::new(
        "id-ollama".to_string(),
        "Llama3".to_string(),
        ProviderType::Ollama,
        "llama3.2:latest".to_string(),
    );
    models.add_model(openrouter_model);
    models.add_model(ollama_model);
    assert_eq!(models.models().len(), 2);

    // Filter by provider (used in chatty-gpui's settings view and message send path)
    let openrouter_only = models.models_by_provider(&ProviderType::OpenRouter);
    assert_eq!(openrouter_only.len(), 1);
    assert_eq!(openrouter_only[0].id, "id-openrouter");

    // Update model settings (simulating user editing settings and saving)
    let mut updated = models.get_model("id-openrouter").unwrap().clone();
    updated.preamble = "You are a helpful assistant.".to_string();
    updated.temperature = 0.5;
    assert!(models.update_model(updated));

    let model = models.get_model("id-openrouter").unwrap();
    assert_eq!(model.preamble, "You are a helpful assistant.");
    assert!((model.temperature - 0.5).abs() < f32::EPSILON);

    // Update non-existent model returns false
    let ghost = ModelConfig::new(
        "nonexistent".to_string(),
        "Ghost".to_string(),
        ProviderType::OpenRouter,
        "openrouter/ghost".to_string(),
    );
    assert!(!models.update_model(ghost));

    // Delete model
    assert!(models.delete_model("id-ollama"));
    assert_eq!(models.models().len(), 1);
    assert!(models.get_model("id-ollama").is_none());

    // Delete non-existent model returns false
    assert!(!models.delete_model("id-ollama"));

    // Replace all (used when loading models from disk)
    let fresh = vec![ModelConfig::new(
        "id-new".to_string(),
        "GPT-4o (via OpenRouter)".to_string(),
        ProviderType::OpenRouter,
        "openai/gpt-4o".to_string(),
    )];
    models.replace_all(fresh);
    assert_eq!(models.models().len(), 1);
    assert!(models.get_model("id-openrouter").is_none());
    assert!(models.get_model("id-new").is_some());
}

// ── ConversationsStore: full lifecycle as chatty-gpui performs it ──────────────

/// Tests the conversation store lifecycle: load metadata, set active, navigate,
/// upsert on title update, delete, and fallback behaviour.
#[test]
fn conversations_store_full_lifecycle() {
    let mut store = ConversationsStore::new();
    assert_eq!(store.count(), 0);
    assert!(store.active_id().is_none());

    // Load metadata at startup (most recent conversations first when rendered)
    store.upsert_metadata("conv-1", "Chat about Rust", 0.005, 1_700_000_000);
    store.upsert_metadata("conv-2", "Chat about Python", 0.003, 1_700_001_000);
    store.upsert_metadata("conv-3", "Chat about Go", 0.001, 1_699_999_000);
    assert_eq!(store.count(), 3);

    // Most recent first in sidebar
    let list = store.list_recent_metadata(10);
    assert_eq!(list.len(), 3);
    assert_eq!(list[0].0, "conv-2"); // updated_at=1_700_001_000 is newest

    // set_active validates against metadata: succeeds for known ID, fails for unknown
    assert!(store.set_active("conv-2".to_string()));
    assert_eq!(store.active_id().unwrap(), "conv-2");
    assert!(!store.set_active("does-not-exist".to_string()));
    assert_eq!(
        store.active_id().unwrap(),
        "conv-2",
        "Active unchanged on failed set"
    );

    // set_active_by_id skips validation (used right after creating a new conversation)
    store.set_active_by_id("conv-1".to_string());
    assert_eq!(store.active_id().unwrap(), "conv-1");

    // Upsert updates existing entry and re-sorts (e.g. after title generation or cost update)
    store.upsert_metadata("conv-3", "Go generics deep dive", 0.008, 1_700_002_000);
    let list = store.list_recent_metadata(1);
    assert_eq!(
        list[0].0, "conv-3",
        "conv-3 should be first after timestamp update"
    );
    assert_eq!(
        list[0].1, "Go generics deep dive",
        "Title should be updated"
    );

    // Delete active conversation: active should shift to the next most recent
    store.set_active_by_id("conv-2".to_string());
    assert!(store.delete_conversation("conv-2"));
    assert!(
        store.active_id().is_some(),
        "Active should fallback after deletion"
    );
    assert_ne!(
        store.active_id().unwrap(),
        "conv-2",
        "Deleted conv should not be active"
    );
    assert_eq!(store.count(), 2);

    // Delete a non-active conversation
    assert!(store.delete_conversation("conv-1"));
    assert_eq!(store.count(), 1);
    assert!(
        !store.delete_conversation("conv-1"),
        "Second delete should return false"
    );

    // is_loaded: false before data is fetched from DB
    assert!(!store.is_loaded("conv-3"));
}

/// Tests that all_metadata_ids returns all IDs sorted most-recent-first across
/// a large number of entries (used for keyboard navigation in chatty-gpui).
#[test]
fn conversations_store_all_ids_ordered() {
    let mut store = ConversationsStore::new();
    let n = 100usize;
    for i in 0..n {
        store.upsert_metadata(&format!("conv-{i}"), &format!("Title {i}"), 0.0, i as i64);
    }

    let ids = store.all_metadata_ids();
    assert_eq!(ids.len(), n);
    assert_eq!(ids[0], format!("conv-{}", n - 1), "Newest first");
    assert_eq!(ids[n - 1], "conv-0", "Oldest last");
}

// ── Provider capability defaults propagated to ModelConfig ────────────────────

/// Ensures ProviderType::default_capabilities() covers all provider variants and
/// that the values can be stored directly in ModelConfig (as models_controller does).
#[test]
fn provider_default_capabilities_propagate_to_model_config() {
    // (provider, expected_images, expected_pdf)
    let cases = [
        (ProviderType::OpenRouter, true, true),
        (ProviderType::AzureOpenAI, true, false),
        (ProviderType::Ollama, false, false),
    ];

    for (provider_type, expected_images, expected_pdf) in cases {
        let (supports_images, supports_pdf) = provider_type.default_capabilities();
        assert_eq!(
            supports_images, expected_images,
            "{:?}: unexpected supports_images",
            provider_type
        );
        assert_eq!(
            supports_pdf, expected_pdf,
            "{:?}: unexpected supports_pdf",
            provider_type
        );

        // Create a ModelConfig using the defaults (same path as models_controller)
        let mut model = ModelConfig::new(
            "test-id".to_string(),
            "Test Model".to_string(),
            provider_type.clone(),
            "test-identifier".to_string(),
        );
        model.supports_images = supports_images;
        model.supports_pdf = supports_pdf;

        assert_eq!(model.supports_images, expected_images);
        assert_eq!(model.supports_pdf, expected_pdf);
    }
}

// ── Azure provider: complex configuration requirements ────────────────────────

/// Azure OpenAI requires both a base_url and credentials (API key or Entra ID).
/// This is the exact logic used in chatty-gpui's app_controller when deciding
/// whether to build an AgentClient for a provider.
#[test]
fn azure_provider_configured_providers_filter() {
    let mut provider_model = ProviderModel::new();

    let mut fully_configured =
        ProviderConfig::new("Azure API Key".to_string(), ProviderType::AzureOpenAI);
    fully_configured.base_url = Some("https://mydeployment.openai.azure.com".to_string());
    fully_configured.api_key = Some("secret-key".to_string());

    let mut entra_configured =
        ProviderConfig::new("Azure Entra".to_string(), ProviderType::AzureOpenAI);
    entra_configured.base_url = Some("https://mydeployment.openai.azure.com".to_string());
    entra_configured.set_azure_auth_method(AzureAuthMethod::EntraId);

    let mut missing_endpoint =
        ProviderConfig::new("Azure No URL".to_string(), ProviderType::AzureOpenAI);
    missing_endpoint.api_key = Some("secret-key".to_string());
    // base_url intentionally not set

    let mut missing_creds =
        ProviderConfig::new("Azure No Key".to_string(), ProviderType::AzureOpenAI);
    missing_creds.base_url = Some("https://mydeployment.openai.azure.com".to_string());
    // Neither api_key nor EntraId configured

    provider_model.add_provider(fully_configured);
    provider_model.add_provider(entra_configured);
    provider_model.add_provider(missing_endpoint);
    provider_model.add_provider(missing_creds);

    let configured: Vec<_> = provider_model.configured_providers().collect();
    assert_eq!(
        configured.len(),
        2,
        "Only fully-configured Azure providers should pass"
    );
    let names: Vec<&str> = configured.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"Azure API Key"));
    assert!(names.contains(&"Azure Entra"));
}

/// Ollama is always considered configured (no API key required, connects locally).
#[test]
fn ollama_provider_always_configured() {
    let mut provider_model = ProviderModel::new();
    // Ollama with no API key — should still be in configured_providers
    let ollama = ProviderConfig::new("Local Ollama".to_string(), ProviderType::Ollama);
    provider_model.add_provider(ollama);

    assert_eq!(provider_model.configured_providers().count(), 1);
}

// ── Serialization roundtrips ──────────────────────────────────────────────────

/// ModelConfig must survive a JSON roundtrip as it is loaded from and saved to disk.
#[test]
fn model_config_json_roundtrip() {
    let mut model = ModelConfig::new(
        "id-gpt4o".to_string(),
        "GPT-4o (via OpenRouter)".to_string(),
        ProviderType::OpenRouter,
        "openai/gpt-4o".to_string(),
    );
    model.supports_images = true;
    model.supports_pdf = false;
    model.supports_temperature = true;
    model.temperature = 0.7;
    model.preamble = "You are a helpful assistant.".to_string();
    model.max_tokens = Some(4096);
    model.cost_per_million_input_tokens = Some(5.0);
    model.cost_per_million_output_tokens = Some(15.0);
    model.max_context_window = Some(128_000);

    let json = serde_json::to_string(&model).expect("ModelConfig serialization failed");
    let restored: ModelConfig =
        serde_json::from_str(&json).expect("ModelConfig deserialization failed");

    assert_eq!(restored.id, model.id);
    assert_eq!(restored.name, model.name);
    assert_eq!(restored.model_identifier, model.model_identifier);
    assert_eq!(restored.supports_images, model.supports_images);
    assert_eq!(restored.supports_pdf, model.supports_pdf);
    assert_eq!(restored.supports_temperature, model.supports_temperature);
    assert!((restored.temperature - model.temperature).abs() < f32::EPSILON);
    assert_eq!(restored.preamble, model.preamble);
    assert_eq!(restored.max_tokens, model.max_tokens);
    assert_eq!(
        restored.cost_per_million_input_tokens,
        model.cost_per_million_input_tokens
    );
    assert_eq!(
        restored.cost_per_million_output_tokens,
        model.cost_per_million_output_tokens
    );
    assert_eq!(restored.max_context_window, model.max_context_window);
}

/// ProviderConfig must survive a JSON roundtrip — includes api_key preservation.
#[test]
fn provider_config_json_roundtrip() {
    let provider = ProviderConfig::new("OpenRouter Prod".to_string(), ProviderType::OpenRouter)
        .with_api_key("sk-or-api-abc123".to_string())
        .with_base_url("https://openrouter.ai/api/v1".to_string());

    let json = serde_json::to_string(&provider).expect("ProviderConfig serialization failed");
    let restored: ProviderConfig =
        serde_json::from_str(&json).expect("ProviderConfig deserialization failed");

    assert_eq!(restored.name, provider.name);
    assert_eq!(restored.provider_type, provider.provider_type);
    assert_eq!(restored.api_key, provider.api_key);
    assert_eq!(restored.base_url, provider.base_url);
}

/// GeneralSettingsModel must survive a JSON roundtrip.
#[test]
fn general_settings_model_json_roundtrip() {
    let mut settings = GeneralSettingsModel::default();
    // Defaults
    assert!((settings.font_size - 14.0).abs() < f32::EPSILON);
    assert!(settings.theme_name.is_none());
    assert!(settings.dark_mode.is_none());
    assert!(!settings.show_tool_traces_live);

    // With user preferences
    settings.font_size = 16.0;
    settings.theme_name = Some("catppuccin".to_string());
    settings.dark_mode = Some(true);
    settings.show_tool_traces_live = true;

    let json = serde_json::to_string(&settings).expect("GeneralSettingsModel serialization failed");
    let restored: GeneralSettingsModel =
        serde_json::from_str(&json).expect("GeneralSettingsModel deserialization failed");

    assert!((restored.font_size - 16.0).abs() < f32::EPSILON);
    assert_eq!(restored.theme_name.as_deref(), Some("catppuccin"));
    assert_eq!(restored.dark_mode, Some(true));
    assert!(restored.show_tool_traces_live);
}

// ── Token budget: snapshot calculations used by chatty-gpui's UI ──────────────

/// TokenBudgetSnapshot status thresholds tested with a realistic model context
/// window (GPT-4, 128k tokens). Mirrors chatty-gpui's token_budget_manager behaviour.
#[test]
fn token_budget_status_thresholds() {
    let limit = 128_000usize;
    let reserve = 4_096usize;
    let effective = limit - reserve; // 123_904

    // < 50% → Normal
    let snap = make_snapshot(effective / 5, limit, reserve);
    assert_eq!(snap.status(), ContextStatus::Normal);

    // 50–70% → Moderate
    let snap = make_snapshot((effective as f64 * 0.60) as usize, limit, reserve);
    assert_eq!(snap.status(), ContextStatus::Moderate);

    // 70–90% → High
    let snap = make_snapshot((effective as f64 * 0.80) as usize, limit, reserve);
    assert_eq!(snap.status(), ContextStatus::High);

    // > 90% → Critical
    let snap = make_snapshot((effective as f64 * 0.95) as usize, limit, reserve);
    assert_eq!(snap.status(), ContextStatus::Critical);
}

/// Verify ContextStatus warning/critical flags — used in chatty-gpui to colour
/// the context window fill bar.
#[test]
fn context_status_warning_flags() {
    assert!(!ContextStatus::Normal.is_warning());
    assert!(!ContextStatus::Moderate.is_warning());
    assert!(ContextStatus::High.is_warning());
    assert!(ContextStatus::Critical.is_warning());

    assert!(!ContextStatus::Normal.is_critical());
    assert!(!ContextStatus::Moderate.is_critical());
    assert!(!ContextStatus::High.is_critical());
    assert!(ContextStatus::Critical.is_critical());
}

/// effective_budget must never underflow when reserve > model_limit.
#[test]
fn token_budget_effective_budget_never_underflows() {
    let snap = make_snapshot(0, 1_000, 5_000);
    assert_eq!(snap.effective_budget(), 0, "Should saturate at 0");
    assert_eq!(snap.remaining(), 0, "remaining should also be 0");
}

/// When actual token counts are available (post-response), estimation_delta
/// computes the difference for debugging.
#[test]
fn token_budget_estimation_delta_with_actuals() {
    let mut snap = make_snapshot(8_100, 128_000, 4_096);
    // No actuals yet
    assert!(snap.estimation_delta().is_none());
    assert!(!snap.has_actuals());

    // Provider returns actual counts after response
    snap.actual_input_tokens = Some(8_500);
    snap.actual_output_tokens = Some(300);
    assert!(snap.has_actuals());
    let delta = snap.estimation_delta().unwrap();
    // We estimated 8_100, provider said 8_500 → under-estimated by 400
    assert_eq!(delta, 400);
}

// ── Additional serialization roundtrips ──────────────────────────────────────

/// ExecutionSettingsModel must survive a JSON roundtrip.
#[test]
fn execution_settings_json_roundtrip() {
    use chatty_core::settings::models::ExecutionSettingsModel;

    let mut settings = ExecutionSettingsModel::default();
    settings.filesystem_read_enabled = true;
    settings.filesystem_write_enabled = false;
    settings.git_enabled = true;

    let json = serde_json::to_string(&settings).expect("serialization failed");
    let loaded: ExecutionSettingsModel =
        serde_json::from_str(&json).expect("deserialization failed");

    assert_eq!(loaded.filesystem_read_enabled, true);
    assert_eq!(loaded.filesystem_write_enabled, false);
    assert_eq!(loaded.git_enabled, true);
}

/// SearchSettingsModel must survive a JSON roundtrip.
#[test]
fn search_settings_json_roundtrip() {
    use chatty_core::settings::models::SearchSettingsModel;

    let mut settings = SearchSettingsModel::default();
    settings.tavily_api_key = Some("tavily-key-123".to_string());
    settings.enabled = true;

    let json = serde_json::to_string(&settings).expect("serialization failed");
    let loaded: SearchSettingsModel = serde_json::from_str(&json).expect("deserialization failed");

    assert_eq!(loaded.tavily_api_key, Some("tavily-key-123".to_string()));
    assert_eq!(loaded.enabled, true);
}

/// TrainingSettingsModel must survive a JSON roundtrip.
#[test]
fn training_settings_json_roundtrip() {
    use chatty_core::settings::models::TrainingSettingsModel;

    let settings = TrainingSettingsModel::default();
    let json = serde_json::to_string(&settings).expect("serialization failed");
    let _loaded: TrainingSettingsModel =
        serde_json::from_str(&json).expect("deserialization failed");
}

/// HiveSettingsModel must survive a JSON roundtrip.
#[test]
fn hive_settings_json_roundtrip() {
    use chatty_core::settings::models::HiveSettingsModel;

    let mut settings = HiveSettingsModel::default();
    settings.token = Some("hive-token-abc".to_string());

    let json = serde_json::to_string(&settings).expect("serialization failed");
    let loaded: HiveSettingsModel = serde_json::from_str(&json).expect("deserialization failed");

    assert_eq!(loaded.token, Some("hive-token-abc".to_string()));
}

/// ModelConfig deserialization handles missing optional fields (backward compat).
#[test]
fn model_config_missing_optional_fields_use_defaults() {
    // Minimal JSON — only required fields
    let json = r#"{
        "id": "test",
        "name": "Test Model",
        "provider_type": "open_a_i",
        "model_identifier": "gpt-4"
    }"#;

    let config: ModelConfig = serde_json::from_str(json).expect("deserialization failed");
    assert_eq!(config.id, "test");
    assert_eq!(config.model_identifier, "gpt-4");
    // Optional fields should default
    assert!(!config.supports_images);
    assert!(!config.supports_pdf);
}
