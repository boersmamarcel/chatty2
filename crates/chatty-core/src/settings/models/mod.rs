pub mod a2a_store;
pub mod execution_settings;
pub mod extensions_store;
pub mod general_model;
pub mod hive_settings;
pub mod mcp_store;
pub mod models_store;
pub mod module_settings;
pub mod providers_store;
pub mod search_settings;
pub mod token_tracking_settings;
pub mod training_settings;
pub mod user_secrets_store;

pub use a2a_store::A2aAgentsModel;
pub use execution_settings::ExecutionSettingsModel;
pub use extensions_store::ExtensionsModel;
pub use general_model::GeneralSettingsModel;
pub use hive_settings::HiveSettingsModel;
pub use mcp_store::McpServersModel;
pub use models_store::ModelsModel;
pub use module_settings::ModuleSettingsModel;
pub use providers_store::ProviderModel;
pub use search_settings::SearchSettingsModel;
pub use token_tracking_settings::TokenTrackingSettings;
pub use training_settings::TrainingSettingsModel;
pub use user_secrets_store::UserSecretsModel;

#[cfg(test)]
mod schema_docs {
    use super::ExtensionsModel;
    use super::a2a_store::A2aAgentConfig;
    use super::execution_settings::{ApprovalMode, ExecutionSettingsModel};
    use super::extensions_store::{ExtensionKind, ExtensionSource, InstalledExtension};
    use super::general_model::GeneralSettingsModel;
    use super::hive_settings::HiveSettingsModel;
    use super::mcp_store::{MASKED_API_KEY_SENTINEL, McpServerConfig};
    use super::models_store::ModelConfig;
    use super::module_settings::ModuleSettingsModel;
    use super::providers_store::{ProviderConfig, ProviderType};
    use super::search_settings::{SearchProvider, SearchSettingsModel};
    use super::token_tracking_settings::TokenTrackingSettings;
    use super::training_settings::TrainingSettingsModel;
    use super::user_secrets_store::UserSecretsModel;

    #[test]
    fn documented_enum_json_spellings() {
        assert_eq!(
            serde_json::to_string(&ApprovalMode::AlwaysAsk).unwrap(),
            "\"AlwaysAsk\""
        );
        assert_eq!(
            serde_json::to_string(&ApprovalMode::AutoApproveSandboxed).unwrap(),
            "\"AutoApproveSandboxed\""
        );
        assert_eq!(
            serde_json::to_string(&ApprovalMode::AutoApproveAll).unwrap(),
            "\"AutoApproveAll\""
        );
        assert_eq!(
            serde_json::to_string(&SearchProvider::Tavily).unwrap(),
            "\"Tavily\""
        );
        assert_eq!(
            serde_json::to_string(&SearchProvider::Brave).unwrap(),
            "\"Brave\""
        );
        assert_eq!(
            serde_json::to_string(&ProviderType::OpenRouter).unwrap(),
            "\"open_router\""
        );
        assert_eq!(
            serde_json::from_str::<ProviderType>("\"open_router\"").unwrap(),
            ProviderType::OpenRouter
        );
        assert_eq!(
            serde_json::from_str::<ProviderType>("\"open_ai\"").unwrap(),
            ProviderType::OpenRouter
        );
        assert!(serde_json::from_str::<ProviderType>("\"openrouter\"").is_err());
        assert_eq!(
            serde_json::to_string(&ProviderType::Ollama).unwrap(),
            "\"ollama\""
        );
        assert_eq!(
            serde_json::to_string(&ProviderType::AzureOpenAI).unwrap(),
            "\"azure_openai\""
        );
        assert_eq!(MASKED_API_KEY_SENTINEL, "****");
    }

    #[test]
    fn documented_object_defaults_roundtrip() {
        let general = serde_json::to_value(GeneralSettingsModel::default()).unwrap();
        assert_eq!(general["font_size"], 14.0);
        assert!(general["theme_name"].is_null());
        assert!(general["dark_mode"].is_null());

        let exec = serde_json::to_value(ExecutionSettingsModel::default()).unwrap();
        assert_eq!(exec["enabled"], false);
        assert_eq!(exec["approval_mode"], "AlwaysAsk");
        assert!(exec["workspace_dir"].is_null());
        assert_eq!(exec["filesystem_read_enabled"], true);
        assert_eq!(exec["filesystem_write_enabled"], true);
        assert_eq!(exec["fetch_enabled"], true);
        assert_eq!(exec["git_enabled"], false);
        assert_eq!(exec["execute_code_enabled"], false);
        assert_eq!(exec["docker_code_execution_enabled"], false);
        assert!(exec["docker_host"].is_null());
        assert_eq!(exec["timeout_seconds"], 30);
        assert_eq!(exec["max_output_bytes"], 51200);
        assert_eq!(exec["network_isolation"], false);
        assert_eq!(exec["max_agent_turns"], 10);
        assert_eq!(exec["memory_enabled"], true);
        assert_eq!(exec["embedding_enabled"], false);
        assert!(exec["embedding_provider"].is_null());
        assert!(exec["embedding_model"].is_null());

        let search = serde_json::to_value(SearchSettingsModel::default()).unwrap();
        assert_eq!(search["enabled"], false);
        assert_eq!(search["active_provider"], "Tavily");
        assert!(search["tavily_api_key"].is_null());
        assert!(search["brave_api_key"].is_null());
        assert_eq!(search["max_results"], 5);
        assert_eq!(search["browser_use_enabled"], true);
        assert!(search["browser_use_api_key"].is_null());
        assert_eq!(search["daytona_enabled"], true);
        assert!(search["daytona_api_key"].is_null());

        let training = serde_json::to_value(TrainingSettingsModel::default()).unwrap();
        assert_eq!(training["atif_auto_export"], false);
        assert_eq!(training["jsonl_auto_export"], false);

        let secrets = serde_json::to_value(UserSecretsModel::default()).unwrap();
        assert_eq!(secrets["secrets"], serde_json::json!([]));
        assert!(secrets.get("revealed_keys").is_none());

        let hive = serde_json::to_value(HiveSettingsModel::default()).unwrap();
        assert_eq!(hive["registry_url"], "http://localhost:8080");
        assert_eq!(hive["runner_url"], "http://localhost:8081");
        assert!(hive["token"].is_null());
        assert!(hive["username"].is_null());
        assert!(hive["email"].is_null());

        let extensions = serde_json::to_value(ExtensionsModel::default()).unwrap();
        assert_eq!(extensions["extensions"], serde_json::json!([]));
        assert!(extensions.get("mcp_auth_statuses").is_none());
        assert!(extensions.get("a2a_statuses").is_none());

        let modules = serde_json::to_value(ModuleSettingsModel::default()).unwrap();
        assert_eq!(modules["enabled"], false);
        assert_eq!(modules["gateway_port"], 8420);
        let dir = modules["module_dir"].as_str().unwrap();
        assert!(
            dir.ends_with("chatty/modules") || dir.ends_with("chatty\\modules"),
            "module_dir={dir}"
        );

        let tokens = serde_json::to_value(TokenTrackingSettings::default()).unwrap();
        assert_eq!(tokens["enabled"], true);
        assert_eq!(tokens["response_reserve"], 4096);
        assert_eq!(tokens["high_threshold"], 0.70);
        assert_eq!(tokens["critical_threshold"], 0.90);
        assert_eq!(tokens["auto_summarize"], false);
        assert!(tokens.get("summarization_model_id").is_none());
    }

    #[test]
    fn documented_list_item_skip_and_tags() {
        let provider = ProviderConfig::new("x".into(), ProviderType::Ollama);
        let json = serde_json::to_value(&provider).unwrap();
        assert!(json.get("api_key").is_none());
        assert_eq!(json["provider_type"], "ollama");

        let model = ModelConfig::new(
            "id".into(),
            "n".into(),
            ProviderType::OpenRouter,
            "m".into(),
        );
        let json = serde_json::to_value(&model).unwrap();
        assert_eq!(json["temperature"], 1.0);
        assert_eq!(json["supports_images"], false);
        assert_eq!(json["supports_pdf"], false);
        assert_eq!(json["supports_temperature"], true);
        assert!(json.get("max_tokens").is_none());

        let mcp = McpServerConfig {
            name: "s".into(),
            url: "http://localhost:3000/mcp".into(),
            api_key: None,
            enabled: true,
            is_module: false,
        };
        let json = serde_json::to_value(&mcp).unwrap();
        assert!(json.get("api_key").is_none());
        assert!(json.get("is_module").is_none());
        assert!(json.get("env").is_none());

        let a2a = A2aAgentConfig {
            name: "a".into(),
            url: "https://example.com".into(),
            api_key: None,
            enabled: true,
            skills: vec![],
        };
        let json = serde_json::to_value(&a2a).unwrap();
        assert!(json.get("api_key").is_none());
        assert!(json.get("skills").is_none());

        let ext = InstalledExtension {
            id: "github-mcp".into(),
            display_name: "GitHub".into(),
            description: "d".into(),
            kind: ExtensionKind::WasmModule,
            source: ExtensionSource::Custom,
            pricing_model: None,
            enabled: true,
        };
        let json = serde_json::to_value(&ext).unwrap();
        assert_eq!(json["kind"], serde_json::json!({"kind": "wasm"}));
        assert_eq!(json["source"], serde_json::json!({"type": "custom"}));
    }
}
