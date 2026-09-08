//! Conformance suite for the repository traits behind `RepositoryRegistry`
//! (`crate::lib::RepositoryRegistry`), plus [`ConversationRepository`].
//!
//! Exported behind the `test-support` feature — the same seam
//! `services::stream_fixtures` uses — so an out-of-tree store-backed
//! implementation (e.g. hive's) can run the identical suite from a
//! dev-dependency and be proven equivalent to the JSON/SQLite backends
//! shipped here (AGE-280). Comparisons go through `serde_json::Value` rather
//! than `PartialEq`/`Debug` so this suite needs no changes to the settings
//! model structs.

use crate::settings::models::a2a_store::A2aAgentConfig;
use crate::settings::models::execution_settings::ExecutionSettingsModel;
use crate::settings::models::extensions_store::ExtensionsModel;
use crate::settings::models::general_model::GeneralSettingsModel;
use crate::settings::models::hive_settings::HiveSettingsModel;
use crate::settings::models::mcp_store::McpServerConfig;
use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::module_settings::ModuleSettingsModel;
use crate::settings::models::providers_store::ProviderConfig;
use crate::settings::models::search_settings::SearchSettingsModel;
use crate::settings::models::training_settings::TrainingSettingsModel;
use crate::settings::models::user_secrets_store::UserSecretsModel;
use crate::settings::repositories::{
    A2aRepository, ExecutionSettingsRepository, ExtensionsRepository, GeneralSettingsRepository,
    HiveSettingsRepository, McpRepository, ModelsRepository, ModuleSettingsRepository,
    ProviderRepository, SearchSettingsRepository, TrainingSettingsRepository,
    UserSecretsRepository,
};

use super::conversation_repository::{ConversationData, ConversationRepository};

// ── Single-object settings conformance (load / save) ─────────────────────────

/// Generates a conformance function bound to a single-object settings
/// repository trait (`load`/`save`). Exercises:
/// - **create**: a fresh repository loads the model's `Default`.
/// - **read/update**: a full round trip of every field through `save` then
///   `load`.
/// - a second `save` replaces the previous value rather than merging into
///   it.
macro_rules! single_settings_conformance {
    ($fn_name:ident, $Trait:path, $Model:ty) => {
        pub async fn $fn_name<R>(make: impl Fn() -> R, sample: $Model)
        where
            R: $Trait,
        {
            let repo = make();

            let fresh = repo.load().await.expect("load on a fresh repository");
            assert_eq!(
                serde_json::to_value(&fresh).expect("serialize fresh"),
                serde_json::to_value(<$Model>::default()).expect("serialize default"),
                "a fresh repository must load the model's default value"
            );

            repo.save(sample.clone())
                .await
                .expect("save the sample value");
            let loaded = repo.load().await.expect("load after save");
            assert_eq!(
                serde_json::to_value(&loaded).expect("serialize loaded"),
                serde_json::to_value(&sample).expect("serialize sample"),
                "load after save must round-trip every field"
            );

            repo.save(<$Model>::default())
                .await
                .expect("save the default value (update)");
            let reset = repo.load().await.expect("load after update");
            assert_eq!(
                serde_json::to_value(&reset).expect("serialize reset"),
                serde_json::to_value(<$Model>::default()).expect("serialize default"),
                "save must overwrite the previous value, not merge into it"
            );
        }
    };
}

single_settings_conformance!(
    conformance_general_settings,
    GeneralSettingsRepository,
    GeneralSettingsModel
);
single_settings_conformance!(
    conformance_execution_settings,
    ExecutionSettingsRepository,
    ExecutionSettingsModel
);
single_settings_conformance!(
    conformance_search_settings,
    SearchSettingsRepository,
    SearchSettingsModel
);
single_settings_conformance!(
    conformance_training_settings,
    TrainingSettingsRepository,
    TrainingSettingsModel
);
single_settings_conformance!(
    conformance_user_secrets,
    UserSecretsRepository,
    UserSecretsModel
);
single_settings_conformance!(
    conformance_hive_settings,
    HiveSettingsRepository,
    HiveSettingsModel
);
single_settings_conformance!(
    conformance_extensions,
    ExtensionsRepository,
    ExtensionsModel
);
single_settings_conformance!(
    conformance_module_settings,
    ModuleSettingsRepository,
    ModuleSettingsModel
);

// ── List-based settings conformance (load_all / save_all) ────────────────────

/// Generates a conformance function bound to a list-based settings
/// repository trait (`load_all`/`save_all`). Exercises:
/// - **create**: a fresh repository is empty.
/// - **read/list**: a full round trip of every field across an ordered list.
/// - **update**: `save_all` replaces the whole list rather than merging.
/// - **delete**: `save_all([])` clears it.
macro_rules! list_settings_conformance {
    ($fn_name:ident, $Trait:path, $Model:ty) => {
        pub async fn $fn_name<R>(make: impl Fn() -> R, item_a: $Model, item_b: $Model)
        where
            R: $Trait,
        {
            let repo = make();

            let fresh = repo
                .load_all()
                .await
                .expect("load_all on a fresh repository");
            assert!(fresh.is_empty(), "a fresh repository must start empty");

            repo.save_all(vec![item_a.clone(), item_b.clone()])
                .await
                .expect("save_all the sample items");
            let loaded = repo.load_all().await.expect("load_all after save_all");
            assert_eq!(
                serde_json::to_value(&loaded).expect("serialize loaded"),
                serde_json::to_value(&vec![item_a.clone(), item_b.clone()])
                    .expect("serialize items"),
                "load_all after save_all must round-trip every field, in order"
            );

            repo.save_all(vec![item_b.clone()])
                .await
                .expect("save_all with a shorter list (update)");
            let updated = repo.load_all().await.expect("load_all after update");
            assert_eq!(
                serde_json::to_value(&updated).expect("serialize updated"),
                serde_json::to_value(&vec![item_b.clone()]).expect("serialize item_b"),
                "save_all must replace the previous list, not merge into it"
            );

            repo.save_all(Vec::<$Model>::new())
                .await
                .expect("save_all([]) (delete)");
            let cleared = repo.load_all().await.expect("load_all after delete");
            assert!(cleared.is_empty(), "save_all([]) must delete every item");
        }
    };
}

list_settings_conformance!(conformance_providers, ProviderRepository, ProviderConfig);
list_settings_conformance!(conformance_models, ModelsRepository, ModelConfig);
list_settings_conformance!(conformance_mcp_servers, McpRepository, McpServerConfig);
list_settings_conformance!(conformance_a2a_agents, A2aRepository, A2aAgentConfig);

// ── Conversation repository conformance ──────────────────────────────────────

/// Build a fully-populated [`ConversationData`] for round-trip testing —
/// every field carries a non-default value.
pub fn sample_conversation(id: &str, title: &str, updated_at: i64) -> ConversationData {
    ConversationData {
        id: id.to_string(),
        title: title.to_string(),
        model_id: "model-1".to_string(),
        message_history: r#"[{"role":"user","content":"hi"}]"#.to_string(),
        system_traces: "[null]".to_string(),
        token_usage: r#"{"total_estimated_cost_usd":0.01}"#.to_string(),
        attachment_paths: r#"[["/tmp/a.png"]]"#.to_string(),
        message_timestamps: format!("[{}]", updated_at),
        message_feedback: "[null]".to_string(),
        regeneration_records: "[]".to_string(),
        created_at: updated_at - 500,
        updated_at,
        working_dir: Some("/tmp/workspace".to_string()),
        agent_task_snapshot: Some(r#"{"todos":[]}"#.to_string()),
        mode: Some(
            r#"{"kind":"hosted","server_url":"http://localhost:8081","remote_id":"remote-1"}"#
                .to_string(),
        ),
    }
}

/// Conformance for [`ConversationRepository`]. Unlike the settings
/// repositories above, this takes an already-constructed `repo` rather than
/// a `make` factory: `ConversationSqliteRepository`'s constructor is async
/// (it opens a pool and runs migrations), so a synchronous factory closure
/// doesn't fit.
///
/// Exercises create, read (including a missing id), list ordering (newest
/// `updated_at` first, for both `load_all` and `load_metadata`), update
/// (re-saving an existing id replaces it rather than duplicating it), and
/// delete.
pub async fn conformance_conversation<R: ConversationRepository>(repo: R) {
    let conv_a = sample_conversation("conv-a", "First", 1_000);
    repo.save("conv-a", conv_a.clone())
        .await
        .expect("save conv-a");

    let loaded = repo
        .load_one("conv-a")
        .await
        .expect("load_one conv-a")
        .expect("conv-a exists");
    assert_eq!(
        serde_json::to_value(&loaded).expect("serialize loaded"),
        serde_json::to_value(&conv_a).expect("serialize conv_a"),
        "load_one must round-trip every field"
    );

    let missing = repo
        .load_one("does-not-exist")
        .await
        .expect("load_one for a missing id");
    assert!(missing.is_none(), "load_one for a missing id must be None");

    let conv_b = sample_conversation("conv-b", "Second", 2_000);
    repo.save("conv-b", conv_b.clone())
        .await
        .expect("save conv-b");

    let all = repo.load_all().await.expect("load_all");
    assert_eq!(all.len(), 2);
    assert_eq!(
        all[0].id, "conv-b",
        "load_all must sort newest-updated first"
    );
    assert_eq!(all[1].id, "conv-a");

    let metadata = repo.load_metadata().await.expect("load_metadata");
    assert_eq!(metadata.len(), 2);
    assert_eq!(
        metadata[0].id, "conv-b",
        "load_metadata must sort newest-updated first"
    );
    assert_eq!(metadata[0].title, "Second");
    assert_eq!(metadata[1].id, "conv-a");

    let mut conv_a_updated = conv_a.clone();
    conv_a_updated.title = "First (edited)".to_string();
    conv_a_updated.updated_at = 3_000;
    repo.save("conv-a", conv_a_updated.clone())
        .await
        .expect("update conv-a");

    let after_update = repo.load_all().await.expect("load_all after update");
    assert_eq!(
        after_update.len(),
        2,
        "saving an existing id must update it, not add a duplicate"
    );
    assert_eq!(after_update[0].id, "conv-a");
    assert_eq!(after_update[0].title, "First (edited)");

    repo.delete("conv-b").await.expect("delete conv-b");
    let remaining = repo.load_all().await.expect("load_all after delete");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, "conv-a");
    assert!(
        repo.load_one("conv-b")
            .await
            .expect("load_one after delete")
            .is_none()
    );
}

// ── Tests: run the suite against this crate's own JSON/SQLite backends ───────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repositories::conversation_sqlite_repository::ConversationSqliteRepository;
    use crate::settings::models::execution_settings::ApprovalMode;
    use crate::settings::models::extensions_store::{
        ExtensionKind, ExtensionSource, InstalledExtension,
    };
    use crate::settings::models::providers_store::ProviderType;
    use crate::settings::models::search_settings::SearchProvider;
    use crate::settings::models::user_secrets_store::UserSecret;
    use crate::settings::repositories::{
        A2aJsonRepository, ExecutionSettingsJsonRepository, ExtensionsJsonRepository,
        GeneralSettingsJsonRepository, HiveSettingsJsonRepository, JsonFileRepository,
        JsonMcpRepository, JsonModelsRepository, ModuleSettingsJsonRepository,
        SearchSettingsJsonRepository, TrainingSettingsJsonRepository, UserSecretsJsonRepository,
    };

    /// A fresh temp file path for one test. The `TempDir` guard must be kept
    /// alive for the test's duration (it deletes the directory on drop).
    fn temp_path(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(name);
        (dir, path)
    }

    #[tokio::test]
    async fn general_settings_json_backend() {
        let (_dir, path) = temp_path("general_settings.json");
        conformance_general_settings(
            || GeneralSettingsJsonRepository::with_path(path.clone()),
            GeneralSettingsModel {
                font_size: 18.5,
                theme_name: Some("Solarized".to_string()),
                dark_mode: Some(true),
            },
        )
        .await;
    }

    #[tokio::test]
    async fn execution_settings_json_backend() {
        let (_dir, path) = temp_path("execution_settings.json");
        conformance_execution_settings(
            || ExecutionSettingsJsonRepository::with_path(path.clone()),
            ExecutionSettingsModel {
                enabled: true,
                approval_mode: ApprovalMode::AutoApproveAll,
                workspace_dir: Some("/workspace".to_string()),
                filesystem_read_enabled: false,
                filesystem_write_enabled: false,
                fetch_enabled: false,
                git_enabled: true,
                browser_enabled: true,
                execute_code_enabled: true,
                docker_code_execution_enabled: true,
                docker_host: Some("unix:///var/run/docker.sock".to_string()),
                timeout_seconds: 120,
                max_output_bytes: 102_400,
                network_isolation: true,
                max_agent_turns: 25,
                memory_enabled: false,
                embedding_enabled: true,
                embedding_provider: Some(ProviderType::OpenRouter),
                embedding_model: Some("text-embedding-3-small".to_string()),
                hosted_conversations_enabled: true,
            },
        )
        .await;
    }

    #[tokio::test]
    async fn search_settings_json_backend() {
        let (_dir, path) = temp_path("search_settings.json");
        conformance_search_settings(
            || SearchSettingsJsonRepository::with_path(path.clone()),
            SearchSettingsModel {
                enabled: true,
                active_provider: SearchProvider::Brave,
                tavily_api_key: Some("tvly-key".to_string()),
                brave_api_key: Some("brave-key".to_string()),
                max_results: 10,
                browser_use_enabled: false,
                browser_use_api_key: Some("bu-key".to_string()),
                daytona_enabled: false,
                daytona_api_key: Some("dt-key".to_string()),
            },
        )
        .await;
    }

    #[tokio::test]
    async fn training_settings_json_backend() {
        let (_dir, path) = temp_path("training_settings.json");
        conformance_training_settings(
            || TrainingSettingsJsonRepository::with_path(path.clone()),
            TrainingSettingsModel {
                atif_auto_export: true,
                jsonl_auto_export: true,
            },
        )
        .await;
    }

    #[tokio::test]
    async fn user_secrets_json_backend() {
        let (_dir, path) = temp_path("user_secrets.json");
        conformance_user_secrets(
            || UserSecretsJsonRepository::with_path(path.clone()),
            UserSecretsModel {
                secrets: vec![UserSecret {
                    key: "API_KEY".to_string(),
                    value: "sekret".to_string(),
                }],
                revealed_keys: Default::default(),
            },
        )
        .await;
    }

    #[tokio::test]
    async fn hive_settings_json_backend() {
        let (_dir, path) = temp_path("hive_settings.json");
        conformance_hive_settings(
            || HiveSettingsJsonRepository::with_path(path.clone()),
            HiveSettingsModel {
                registry_url: "https://hive.example.com".to_string(),
                runner_url: "https://runner.example.com".to_string(),
                token: Some("jwt-token".to_string()),
                username: Some("marcel".to_string()),
                email: Some("marcel@example.com".to_string()),
            },
        )
        .await;
    }

    #[tokio::test]
    async fn extensions_json_backend() {
        let (_dir, path) = temp_path("extensions.json");
        let mut sample = ExtensionsModel::default();
        sample.extensions.push(InstalledExtension {
            id: "github-mcp".to_string(),
            display_name: "GitHub MCP".to_string(),
            description: "GitHub tools".to_string(),
            kind: ExtensionKind::WasmModule,
            source: ExtensionSource::Custom,
            pricing_model: Some("free".to_string()),
            enabled: false,
        });
        conformance_extensions(|| ExtensionsJsonRepository::with_path(path.clone()), sample).await;
    }

    #[tokio::test]
    async fn module_settings_json_backend() {
        let (_dir, path) = temp_path("module_settings.json");
        conformance_module_settings(
            || ModuleSettingsJsonRepository::with_path(path.clone()),
            ModuleSettingsModel {
                enabled: true,
                module_dir: "/opt/chatty-test-modules".to_string(),
                gateway_port: 9000,
            },
        )
        .await;
    }

    #[tokio::test]
    async fn providers_json_backend() {
        let (_dir, path) = temp_path("providers.json");
        conformance_providers(
            || JsonFileRepository::with_path(path.clone()),
            ProviderConfig::new("openrouter".to_string(), ProviderType::OpenRouter)
                .with_api_key("key-a".to_string()),
            ProviderConfig::new("ollama".to_string(), ProviderType::Ollama)
                .with_base_url("http://localhost:11434".to_string()),
        )
        .await;
    }

    #[tokio::test]
    async fn models_json_backend() {
        let (_dir, path) = temp_path("models.json");
        conformance_models(
            || JsonModelsRepository::with_path(path.clone()),
            ModelConfig::new(
                "model-a".to_string(),
                "Model A".to_string(),
                ProviderType::OpenRouter,
                "anthropic/claude".to_string(),
            ),
            ModelConfig::new(
                "model-b".to_string(),
                "Model B".to_string(),
                ProviderType::Ollama,
                "llama3.2".to_string(),
            )
            .synced(),
        )
        .await;
    }

    #[tokio::test]
    async fn mcp_servers_json_backend() {
        let (_dir, path) = temp_path("mcp_servers.json");
        conformance_mcp_servers(
            || JsonMcpRepository::with_path(path.clone()),
            McpServerConfig {
                name: "server-a".to_string(),
                url: "http://localhost:3000/mcp".to_string(),
                api_key: Some("mcp-key".to_string()),
                enabled: true,
                is_module: false,
            },
            McpServerConfig {
                name: "server-b".to_string(),
                url: "http://localhost:3001/mcp".to_string(),
                api_key: None,
                enabled: false,
                is_module: true,
            },
        )
        .await;
    }

    #[tokio::test]
    async fn a2a_agents_json_backend() {
        let (_dir, path) = temp_path("a2a_agents.json");
        conformance_a2a_agents(
            || A2aJsonRepository::with_path(path.clone()),
            A2aAgentConfig {
                name: "agent-a".to_string(),
                url: "https://hive.dev/a2a/agent-a".to_string(),
                api_key: Some("a2a-key".to_string()),
                enabled: true,
                skills: vec!["translate".to_string()],
            },
            A2aAgentConfig {
                name: "agent-b".to_string(),
                url: "https://hive.dev/a2a/agent-b".to_string(),
                api_key: None,
                enabled: false,
                skills: vec![],
            },
        )
        .await;
    }

    #[tokio::test]
    async fn conversation_sqlite_backend() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("conversations.db");
        let repo = ConversationSqliteRepository::with_path(db_path)
            .await
            .expect("open sqlite repository");
        conformance_conversation(repo).await;
    }
}
