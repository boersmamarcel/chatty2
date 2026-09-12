//! `SettingsSnapshot` — a transport-agnostic, byte-stable view over every
//! settings family behind [`RepositoryRegistry`] (AGE-283).
//!
//! A hosted guest never reads a config directory: it is handed a
//! `SettingsSnapshot` at lease (`RepositoryRegistry::from_snapshot`, built
//! entirely in memory) and hands back a [`SettingsDelta`] on release
//! (`registry.delta_since`, written back with `registry.apply`). The
//! transport itself — MMDS vs. the first vsock message — is hive's call, not
//! this crate's; this module only guarantees the bytes are canonical.
//!
//! ## Canonicalization
//!
//! `#[derive(Serialize)]` on a named-field struct always emits fields in
//! declaration order, so most of `SettingsSnapshot` is already byte-stable
//! across processes without any extra work. The exception is two `HashMap`
//! fields nested inside family models (`ProviderConfig::extra_config`,
//! `ModelConfig::extra_params`): this workspace transitively enables
//! serde_json's `preserve_order` feature (pulled in by GPUI), so
//! `serde_json::Map` is `IndexMap`-backed here and does **not** sort itself —
//! serializing those maps directly would emit keys in per-process
//! HashMap-iteration order. Relying on the ambient feature flag would make
//! "byte-identical" an accident of the build graph rather than a guarantee.
//!
//! [`SettingsSnapshot::canonical_bytes`] instead performs an explicit
//! canonicalization pass over `serde_json::Value`: it recursively re-sorts
//! every object's entries lexicographically by key, independent of whichever
//! `Map` backend is active. Arrays are left in their original order — the
//! four list-based families (providers, models, mcp_servers, a2a_agents)
//! carry order that is part of their contract (`store_conformance.rs`
//! exercises it), not something to canonicalize away.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::RepositoryRegistry;
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
    InMemoryA2aRepository, InMemoryExecutionSettingsRepository, InMemoryExtensionsRepository,
    InMemoryGeneralSettingsRepository, InMemoryHiveSettingsRepository, InMemoryMcpRepository,
    InMemoryModelsRepository, InMemoryModuleSettingsRepository, InMemoryProviderRepository,
    InMemorySearchSettingsRepository, InMemoryTrainingSettingsRepository,
    InMemoryUserSecretsRepository, RepositoryResult,
};

/// Recursively re-sort every JSON object's entries by key, in place. Arrays
/// are recursed into but left in their original element order.
fn canonicalize(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(items) => items.iter_mut().for_each(canonicalize),
        serde_json::Value::Object(map) => {
            map.values_mut().for_each(canonicalize);
            let mut entries: Vec<_> = std::mem::take(map).into_iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.cmp(b));
            map.extend(entries);
        }
        _ => {}
    }
}

/// The union of every settings family `RepositoryRegistry` serves. A view
/// over the existing family structs — not a second settings model.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SettingsSnapshot {
    pub a2a_agents: Vec<A2aAgentConfig>,
    pub execution_settings: ExecutionSettingsModel,
    pub extensions: ExtensionsModel,
    pub general_settings: GeneralSettingsModel,
    pub hive_settings: HiveSettingsModel,
    pub mcp_servers: Vec<McpServerConfig>,
    pub models: Vec<ModelConfig>,
    pub module_settings: ModuleSettingsModel,
    pub providers: Vec<ProviderConfig>,
    pub search_settings: SearchSettingsModel,
    pub training_settings: TrainingSettingsModel,
    pub user_secrets: UserSecretsModel,
}

impl SettingsSnapshot {
    /// Serialize with every JSON object's keys in lexicographic order,
    /// independent of `HashMap` iteration order or the `preserve_order`
    /// feature — see the module doc comment.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut value = serde_json::to_value(self).expect("SettingsSnapshot always serializes");
        canonicalize(&mut value);
        serde_json::to_vec(&value).expect("canonicalized value always serializes")
    }
}

/// Families that changed relative to a `SettingsSnapshot`. `None` means that
/// family is unchanged; a hosted guest returns this on lease release instead
/// of a full snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SettingsDelta {
    pub a2a_agents: Option<Vec<A2aAgentConfig>>,
    pub execution_settings: Option<ExecutionSettingsModel>,
    pub extensions: Option<ExtensionsModel>,
    pub general_settings: Option<GeneralSettingsModel>,
    pub hive_settings: Option<HiveSettingsModel>,
    pub mcp_servers: Option<Vec<McpServerConfig>>,
    pub models: Option<Vec<ModelConfig>>,
    pub module_settings: Option<ModuleSettingsModel>,
    pub providers: Option<Vec<ProviderConfig>>,
    pub search_settings: Option<SearchSettingsModel>,
    pub training_settings: Option<TrainingSettingsModel>,
    pub user_secrets: Option<UserSecretsModel>,
}

impl SettingsDelta {
    /// True when every family is unchanged.
    pub fn is_empty(&self) -> bool {
        let Self {
            a2a_agents,
            execution_settings,
            extensions,
            general_settings,
            hive_settings,
            mcp_servers,
            models,
            module_settings,
            providers,
            search_settings,
            training_settings,
            user_secrets,
        } = self;
        a2a_agents.is_none()
            && execution_settings.is_none()
            && extensions.is_none()
            && general_settings.is_none()
            && hive_settings.is_none()
            && mcp_servers.is_none()
            && models.is_none()
            && module_settings.is_none()
            && providers.is_none()
            && search_settings.is_none()
            && training_settings.is_none()
            && user_secrets.is_none()
    }
}

/// `Some(current)` if `current` differs from `original`, `None` otherwise.
/// Compares via `serde_json::Value` equality, which is order-independent —
/// canonicalization is only needed when producing bytes for transport, not
/// for detecting whether a family changed.
fn changed<T>(current: &T, original: &T) -> Option<T>
where
    T: Clone + Serialize,
{
    let current_value = serde_json::to_value(current).expect("value always serializes");
    let original_value = serde_json::to_value(original).expect("value always serializes");
    (current_value != original_value).then(|| current.clone())
}

impl RepositoryRegistry {
    /// Load every family concurrently and assemble a snapshot.
    pub async fn snapshot(&self) -> RepositoryResult<SettingsSnapshot> {
        let (
            a2a_agents,
            execution_settings,
            extensions,
            general_settings,
            hive_settings,
            mcp_servers,
            models,
            module_settings,
            providers,
            search_settings,
            training_settings,
            user_secrets,
        ) = tokio::try_join!(
            self.a2a.load_all(),
            self.execution_settings.load(),
            self.extensions.load(),
            self.general_settings.load(),
            self.hive_settings.load(),
            self.mcp.load_all(),
            self.models.load_all(),
            self.module_settings.load(),
            self.providers.load_all(),
            self.search_settings.load(),
            self.training_settings.load(),
            self.user_secrets.load(),
        )?;

        Ok(SettingsSnapshot {
            a2a_agents,
            execution_settings,
            extensions,
            general_settings,
            hive_settings,
            mcp_servers,
            models,
            module_settings,
            providers,
            search_settings,
            training_settings,
            user_secrets,
        })
    }

    /// Build an in-memory-only registry from a snapshot — the shape a hosted
    /// guest boots with. No disk, no database; every family lives in an
    /// `Arc<RwLock<_>>` for the lease's duration.
    pub fn from_snapshot(snapshot: SettingsSnapshot) -> RepositoryRegistry {
        RepositoryRegistry {
            providers: Arc::new(InMemoryProviderRepository::new(snapshot.providers)),
            general_settings: Arc::new(InMemoryGeneralSettingsRepository::new(
                snapshot.general_settings,
            )),
            models: Arc::new(InMemoryModelsRepository::new(snapshot.models)),
            mcp: Arc::new(InMemoryMcpRepository::new(snapshot.mcp_servers)),
            a2a: Arc::new(InMemoryA2aRepository::new(snapshot.a2a_agents)),
            execution_settings: Arc::new(InMemoryExecutionSettingsRepository::new(
                snapshot.execution_settings,
            )),
            search_settings: Arc::new(InMemorySearchSettingsRepository::new(
                snapshot.search_settings,
            )),
            training_settings: Arc::new(InMemoryTrainingSettingsRepository::new(
                snapshot.training_settings,
            )),
            user_secrets: Arc::new(InMemoryUserSecretsRepository::new(snapshot.user_secrets)),
            module_settings: Arc::new(InMemoryModuleSettingsRepository::new(
                snapshot.module_settings,
            )),
            hive_settings: Arc::new(InMemoryHiveSettingsRepository::new(snapshot.hive_settings)),
            extensions: Arc::new(InMemoryExtensionsRepository::new(snapshot.extensions)),
        }
    }

    /// Families that differ between this registry's current state and
    /// `snapshot`. A hosted guest calls this against its lease-time
    /// `from_snapshot` registry on release, to report back only what
    /// changed during the turn.
    pub async fn delta_since(
        &self,
        snapshot: &SettingsSnapshot,
    ) -> RepositoryResult<SettingsDelta> {
        let current = self.snapshot().await?;
        Ok(SettingsDelta {
            a2a_agents: changed(&current.a2a_agents, &snapshot.a2a_agents),
            execution_settings: changed(&current.execution_settings, &snapshot.execution_settings),
            extensions: changed(&current.extensions, &snapshot.extensions),
            general_settings: changed(&current.general_settings, &snapshot.general_settings),
            hive_settings: changed(&current.hive_settings, &snapshot.hive_settings),
            mcp_servers: changed(&current.mcp_servers, &snapshot.mcp_servers),
            models: changed(&current.models, &snapshot.models),
            module_settings: changed(&current.module_settings, &snapshot.module_settings),
            providers: changed(&current.providers, &snapshot.providers),
            search_settings: changed(&current.search_settings, &snapshot.search_settings),
            training_settings: changed(&current.training_settings, &snapshot.training_settings),
            user_secrets: changed(&current.user_secrets, &snapshot.user_secrets),
        })
    }

    /// Write every changed family in `delta` back through this registry's
    /// repositories. The store-backed side of "return a delta" — a hosted
    /// guest's release path calls this on the real (disk/DB-backed)
    /// registry, not the in-memory lease-time one. Fails fast on the first
    /// repository error; families are independent so callers may retry the
    /// same delta.
    pub async fn apply(&self, delta: SettingsDelta) -> RepositoryResult<()> {
        if let Some(value) = delta.a2a_agents {
            self.a2a.save_all(value).await?;
        }
        if let Some(value) = delta.execution_settings {
            self.execution_settings.save(value).await?;
        }
        if let Some(value) = delta.extensions {
            self.extensions.save(value).await?;
        }
        if let Some(value) = delta.general_settings {
            self.general_settings.save(value).await?;
        }
        if let Some(value) = delta.hive_settings {
            self.hive_settings.save(value).await?;
        }
        if let Some(value) = delta.mcp_servers {
            self.mcp.save_all(value).await?;
        }
        if let Some(value) = delta.models {
            self.models.save_all(value).await?;
        }
        if let Some(value) = delta.module_settings {
            self.module_settings.save(value).await?;
        }
        if let Some(value) = delta.providers {
            self.providers.save_all(value).await?;
        }
        if let Some(value) = delta.search_settings {
            self.search_settings.save(value).await?;
        }
        if let Some(value) = delta.training_settings {
            self.training_settings.save(value).await?;
        }
        if let Some(value) = delta.user_secrets {
            self.user_secrets.save(value).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::models::execution_settings::ApprovalMode;
    use crate::settings::models::providers_store::ProviderType;
    use crate::settings::repositories::{
        A2aJsonRepository, ExecutionSettingsJsonRepository, ExtensionsJsonRepository,
        GeneralSettingsJsonRepository, HiveSettingsJsonRepository, JsonFileRepository,
        JsonMcpRepository, JsonModelsRepository, ModuleSettingsJsonRepository,
        SearchSettingsJsonRepository, TrainingSettingsJsonRepository, UserSecretsJsonRepository,
    };

    /// A `RepositoryRegistry` backed by JSON files under a fresh temp dir.
    /// The `TempDir` guard must outlive the registry.
    fn temp_registry() -> (tempfile::TempDir, RepositoryRegistry) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = |name: &str| dir.path().join(name);
        let registry = RepositoryRegistry {
            providers: Arc::new(JsonFileRepository::with_path(path("providers.json"))),
            general_settings: Arc::new(GeneralSettingsJsonRepository::with_path(path(
                "general_settings.json",
            ))),
            models: Arc::new(JsonModelsRepository::with_path(path("models.json"))),
            mcp: Arc::new(JsonMcpRepository::with_path(path("mcp_servers.json"))),
            a2a: Arc::new(A2aJsonRepository::with_path(path("a2a_agents.json"))),
            execution_settings: Arc::new(ExecutionSettingsJsonRepository::with_path(path(
                "execution_settings.json",
            ))),
            search_settings: Arc::new(SearchSettingsJsonRepository::with_path(path(
                "search_settings.json",
            ))),
            training_settings: Arc::new(TrainingSettingsJsonRepository::with_path(path(
                "training_settings.json",
            ))),
            user_secrets: Arc::new(UserSecretsJsonRepository::with_path(path(
                "user_secrets.json",
            ))),
            module_settings: Arc::new(ModuleSettingsJsonRepository::with_path(path(
                "module_settings.json",
            ))),
            hive_settings: Arc::new(HiveSettingsJsonRepository::with_path(path(
                "hive_settings.json",
            ))),
            extensions: Arc::new(ExtensionsJsonRepository::with_path(path("extensions.json"))),
        };
        (dir, registry)
    }

    #[test]
    fn canonicalize_sorts_object_keys_regardless_of_insertion_order() {
        let a = serde_json::json!({"zebra": 1, "apple": {"delta": 2, "bravo": 3}, "list": [{"y": 1, "x": 2}]});
        let b = serde_json::json!({"apple": {"bravo": 3, "delta": 2}, "zebra": 1, "list": [{"y": 1, "x": 2}]});

        let mut a = a;
        let mut b = b;
        canonicalize(&mut a);
        canonicalize(&mut b);

        assert_eq!(
            serde_json::to_vec(&a).unwrap(),
            serde_json::to_vec(&b).unwrap()
        );
    }

    #[test]
    fn canonicalize_leaves_array_element_order_alone() {
        let mut value = serde_json::json!({"items": [3, 1, 2]});
        canonicalize(&mut value);
        assert_eq!(value["items"], serde_json::json!([3, 1, 2]));
    }

    #[tokio::test]
    async fn delta_is_empty_for_an_unchanged_registry() {
        let (_dir, registry) = temp_registry();
        let snapshot = registry.snapshot().await.expect("snapshot");
        let delta = registry.delta_since(&snapshot).await.expect("delta");
        assert!(
            delta.is_empty(),
            "delta for an unchanged registry must be empty"
        );
    }

    #[tokio::test]
    async fn default_snapshot_round_trips_through_from_snapshot() {
        let snapshot = SettingsSnapshot::default();
        let in_memory = RepositoryRegistry::from_snapshot(snapshot.clone());
        let round_tripped = in_memory
            .snapshot()
            .await
            .expect("snapshot in-memory registry");
        assert_eq!(
            serde_json::to_value(&round_tripped).unwrap(),
            serde_json::to_value(&snapshot).unwrap()
        );
    }

    /// One non-default value per family, reused across the per-family
    /// round-trip test below.
    fn sample_snapshot() -> SettingsSnapshot {
        SettingsSnapshot {
            a2a_agents: vec![A2aAgentConfig {
                name: "agent-a".to_string(),
                url: "https://hive.dev/a2a/agent-a".to_string(),
                api_key: Some("a2a-key".to_string()),
                enabled: true,
                skills: vec!["translate".to_string()],
            }],
            execution_settings: ExecutionSettingsModel {
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
                warn_on_external_agent: true,
                embedding_enabled: true,
                embedding_provider: Some(ProviderType::OpenRouter),
                embedding_model: Some("text-embedding-3-small".to_string()),
                hosted_conversations_enabled: true,
            },
            extensions: {
                let mut model = ExtensionsModel::default();
                model.extensions.push(
                    crate::settings::models::extensions_store::InstalledExtension {
                        id: "github-mcp".to_string(),
                        display_name: "GitHub MCP".to_string(),
                        description: "GitHub tools".to_string(),
                        kind: crate::settings::models::extensions_store::ExtensionKind::WasmModule,
                        source: crate::settings::models::extensions_store::ExtensionSource::Custom,
                        pricing_model: Some("free".to_string()),
                        enabled: false,
                    },
                );
                model
            },
            general_settings: GeneralSettingsModel {
                font_size: 18.5,
                theme_name: Some("Solarized".to_string()),
                dark_mode: Some(true),
            },
            hive_settings: HiveSettingsModel {
                registry_url: "https://hive.example.com".to_string(),
                runner_url: "https://runner.example.com".to_string(),
                token: Some("jwt-token".to_string()),
                username: Some("marcel".to_string()),
                email: Some("marcel@example.com".to_string()),
            },
            mcp_servers: vec![McpServerConfig {
                name: "server-a".to_string(),
                url: "http://localhost:3000/mcp".to_string(),
                api_key: Some("mcp-key".to_string()),
                enabled: true,
                is_module: false,
            }],
            models: vec![ModelConfig::new(
                "model-a".to_string(),
                "Model A".to_string(),
                ProviderType::OpenRouter,
                "anthropic/claude".to_string(),
            )],
            module_settings: ModuleSettingsModel {
                enabled: true,
                module_dir: "/opt/chatty-test-modules".to_string(),
                gateway_port: 9000,
                default_endpoint_budget: 4,
                endpoint_budgets: {
                    let mut budgets = std::collections::HashMap::new();
                    budgets.insert("http://localhost:11434".to_string(), 2);
                    budgets
                },
                virtual_agents: Vec::new(),
            },
            providers: vec![
                ProviderConfig::new("openrouter".to_string(), ProviderType::OpenRouter)
                    .with_api_key("key-a".to_string()),
            ],
            search_settings: SearchSettingsModel {
                enabled: true,
                active_provider: crate::settings::models::search_settings::SearchProvider::Brave,
                tavily_api_key: Some("tvly-key".to_string()),
                brave_api_key: Some("brave-key".to_string()),
                max_results: 10,
                browser_use_enabled: false,
                browser_use_api_key: Some("bu-key".to_string()),
                daytona_enabled: false,
                daytona_api_key: Some("dt-key".to_string()),
            },
            training_settings: TrainingSettingsModel {
                atif_auto_export: true,
                jsonl_auto_export: true,
            },
            user_secrets: UserSecretsModel {
                secrets: vec![crate::settings::models::user_secrets_store::UserSecret {
                    key: "API_KEY".to_string(),
                    value: "sekret".to_string(),
                }],
                revealed_keys: Default::default(),
            },
        }
    }

    /// For each family: mutate only that one family away from default,
    /// diff against a default snapshot, apply the delta to a fresh
    /// store-backed registry, and confirm exactly that family changed.
    macro_rules! family_round_trip_test {
        ($test_name:ident, $field:ident) => {
            #[tokio::test]
            async fn $test_name() {
                let baseline = SettingsSnapshot::default();
                let mut mutated = baseline.clone();
                mutated.$field = sample_snapshot().$field;

                let (_source_dir, source) = temp_registry();
                source
                    .apply(SettingsDelta {
                        $field: Some(mutated.$field.clone()),
                        ..Default::default()
                    })
                    .await
                    .expect("seed the source registry");

                let delta = source.delta_since(&baseline).await.expect("delta_since");
                assert!(
                    delta.$field.is_some(),
                    "the mutated family must appear in the delta"
                );

                let (_dest_dir, dest) = temp_registry();
                dest.apply(delta).await.expect("apply delta to destination");

                let dest_snapshot = dest.snapshot().await.expect("snapshot destination");
                assert_eq!(
                    serde_json::to_value(&dest_snapshot.$field).unwrap(),
                    serde_json::to_value(&mutated.$field).unwrap(),
                    "the mutated family must round-trip through delta_since/apply"
                );

                let mut expected_default = SettingsSnapshot::default();
                expected_default.$field = dest_snapshot.$field.clone();
                assert_eq!(
                    serde_json::to_value(&dest_snapshot).unwrap(),
                    serde_json::to_value(&expected_default).unwrap(),
                    "every other family must stay default"
                );
            }
        };
    }

    family_round_trip_test!(a2a_agents_round_trip, a2a_agents);
    family_round_trip_test!(execution_settings_round_trip, execution_settings);
    family_round_trip_test!(extensions_round_trip, extensions);
    family_round_trip_test!(general_settings_round_trip, general_settings);
    family_round_trip_test!(hive_settings_round_trip, hive_settings);
    family_round_trip_test!(mcp_servers_round_trip, mcp_servers);
    family_round_trip_test!(models_round_trip, models);
    family_round_trip_test!(module_settings_round_trip, module_settings);
    family_round_trip_test!(providers_round_trip, providers);
    family_round_trip_test!(search_settings_round_trip, search_settings);
    family_round_trip_test!(training_settings_round_trip, training_settings);
    family_round_trip_test!(user_secrets_round_trip, user_secrets);
}

/// Cross-process byte-identical serialization (AGE-283's reuse of AGE-276's
/// two-process determinism harness pattern from
/// `factories/agent_factory/tool_block_determinism.rs`). A fixed snapshot
/// fixture is dumped by two independently spawned copies of this same test
/// binary; their `canonical_bytes()` output must match byte-for-byte.
#[cfg(test)]
mod two_process_determinism {
    use super::SettingsSnapshot;
    use std::io::Write;
    use std::process::{Command, Stdio};

    const BEGIN: &str = "CHATTY_SETTINGS_SNAPSHOT_BEGIN";
    const END: &str = "CHATTY_SETTINGS_SNAPSHOT_END";
    const DUMP_ENV: &str = "CHATTY_DUMP_SETTINGS_SNAPSHOT";

    /// A settings snapshot with at least one populated `HashMap` field
    /// (`ProviderConfig::extra_config`), the fields identified as the
    /// concrete byte-order hazard — see the module doc comment.
    fn fixture() -> SettingsSnapshot {
        let mut snapshot = SettingsSnapshot::default();
        let mut provider = crate::settings::models::providers_store::ProviderConfig::new(
            "fixture".to_string(),
            crate::settings::models::providers_store::ProviderType::OpenRouter,
        );
        provider
            .extra_config
            .insert("zebra".to_string(), "1".to_string());
        provider
            .extra_config
            .insert("apple".to_string(), "2".to_string());
        provider
            .extra_config
            .insert("mango".to_string(), "3".to_string());
        snapshot.providers.push(provider);
        snapshot
    }

    /// `#[ignore]`d child entry point: prints `fixture().canonical_bytes()`
    /// as hex between markers, gated by an env var so it never runs as part
    /// of the normal test sweep.
    #[test]
    #[ignore]
    fn dump_fixture_for_parent() {
        if std::env::var(DUMP_ENV).is_err() {
            return;
        }
        let bytes = fixture().canonical_bytes();
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        println!("{BEGIN}");
        println!("{hex}");
        println!("{END}");
        std::io::stdout().flush().ok();
    }

    fn dump_from_child_process() -> Vec<u8> {
        let exe = std::env::current_exe().expect("current test binary");
        let output = Command::new(exe)
            .arg("--exact")
            .arg("settings_snapshot::two_process_determinism::dump_fixture_for_parent")
            .arg("--ignored")
            .arg("--nocapture")
            .env(DUMP_ENV, "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .expect("spawn child test process");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let hex = stdout
            .lines()
            .skip_while(|line| *line != BEGIN)
            .nth(1)
            .unwrap_or_else(|| panic!("child produced no {BEGIN}/{END} block:\n{stdout}"));

        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("valid hex byte"))
            .collect()
    }

    #[test]
    fn snapshot_canonical_bytes_are_byte_identical_across_processes() {
        let first = dump_from_child_process();
        let second = dump_from_child_process();
        assert!(!first.is_empty(), "child must produce non-empty output");
        assert_eq!(
            first, second,
            "SettingsSnapshot::canonical_bytes must be byte-identical across processes"
        );
    }
}
