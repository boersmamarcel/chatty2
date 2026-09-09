//! In-memory implementations of the `RepositoryRegistry` traits.
//!
//! These back [`crate::RepositoryRegistry::from_snapshot`] (AGE-283): a
//! hosted guest never touches disk, so the registry it boots from a
//! `SettingsSnapshot` needs implementations that hold state in a process-local
//! `Arc<RwLock<_>>` instead of a JSON file. Unlike `store_conformance`, this
//! is production code, not test-only — it is not gated behind `test-support`.

use std::sync::{Arc, RwLock};

use super::{BoxFuture, RepositoryResult};

/// Generate an in-memory implementation of a single-object settings
/// repository trait (`load`/`save`).
macro_rules! define_single_in_memory_repository {
    (
        trait $Trait:path,
        struct $StructName:ident,
        model = $Model:ty $(,)?
    ) => {
        pub struct $StructName {
            state: Arc<RwLock<$Model>>,
        }

        impl $StructName {
            pub fn new(initial: $Model) -> Self {
                Self {
                    state: Arc::new(RwLock::new(initial)),
                }
            }
        }

        impl $Trait for $StructName {
            fn load(&self) -> BoxFuture<'static, RepositoryResult<$Model>> {
                let state = self.state.clone();
                Box::pin(async move {
                    Ok(state
                        .read()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .clone())
                })
            }

            fn save(&self, value: $Model) -> BoxFuture<'static, RepositoryResult<()>> {
                let state = self.state.clone();
                Box::pin(async move {
                    *state
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = value;
                    Ok(())
                })
            }
        }
    };
}

/// Generate an in-memory implementation of a list-based settings repository
/// trait (`load_all`/`save_all`).
macro_rules! define_list_in_memory_repository {
    (
        trait $Trait:path,
        struct $StructName:ident,
        model = $Model:ty $(,)?
    ) => {
        pub struct $StructName {
            state: Arc<RwLock<Vec<$Model>>>,
        }

        impl $StructName {
            pub fn new(initial: Vec<$Model>) -> Self {
                Self {
                    state: Arc::new(RwLock::new(initial)),
                }
            }
        }

        impl $Trait for $StructName {
            fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<$Model>>> {
                let state = self.state.clone();
                Box::pin(async move {
                    Ok(state
                        .read()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .clone())
                })
            }

            fn save_all(&self, items: Vec<$Model>) -> BoxFuture<'static, RepositoryResult<()>> {
                let state = self.state.clone();
                Box::pin(async move {
                    *state
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = items;
                    Ok(())
                })
            }
        }
    };
}

// ── Single-object families ────────────────────────────────────────────────

define_single_in_memory_repository!(
    trait super::GeneralSettingsRepository,
    struct InMemoryGeneralSettingsRepository,
    model = crate::settings::models::general_model::GeneralSettingsModel,
);

define_single_in_memory_repository!(
    trait super::ExecutionSettingsRepository,
    struct InMemoryExecutionSettingsRepository,
    model = crate::settings::models::execution_settings::ExecutionSettingsModel,
);

define_single_in_memory_repository!(
    trait super::SearchSettingsRepository,
    struct InMemorySearchSettingsRepository,
    model = crate::settings::models::search_settings::SearchSettingsModel,
);

define_single_in_memory_repository!(
    trait super::TrainingSettingsRepository,
    struct InMemoryTrainingSettingsRepository,
    model = crate::settings::models::training_settings::TrainingSettingsModel,
);

define_single_in_memory_repository!(
    trait super::UserSecretsRepository,
    struct InMemoryUserSecretsRepository,
    model = crate::settings::models::user_secrets_store::UserSecretsModel,
);

define_single_in_memory_repository!(
    trait super::HiveSettingsRepository,
    struct InMemoryHiveSettingsRepository,
    model = crate::settings::models::hive_settings::HiveSettingsModel,
);

define_single_in_memory_repository!(
    trait super::ExtensionsRepository,
    struct InMemoryExtensionsRepository,
    model = crate::settings::models::extensions_store::ExtensionsModel,
);

define_single_in_memory_repository!(
    trait super::ModuleSettingsRepository,
    struct InMemoryModuleSettingsRepository,
    model = crate::settings::models::module_settings::ModuleSettingsModel,
);

// ── List-based families ────────────────────────────────────────────────────

define_list_in_memory_repository!(
    trait super::ProviderRepository,
    struct InMemoryProviderRepository,
    model = crate::settings::models::providers_store::ProviderConfig,
);

define_list_in_memory_repository!(
    trait super::ModelsRepository,
    struct InMemoryModelsRepository,
    model = crate::settings::models::models_store::ModelConfig,
);

define_list_in_memory_repository!(
    trait super::McpRepository,
    struct InMemoryMcpRepository,
    model = crate::settings::models::mcp_store::McpServerConfig,
);

define_list_in_memory_repository!(
    trait super::A2aRepository,
    struct InMemoryA2aRepository,
    model = crate::settings::models::a2a_store::A2aAgentConfig,
);
