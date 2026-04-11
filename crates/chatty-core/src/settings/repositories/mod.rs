//! Settings repositories — persistence layer for Chatty configuration.
//!
//! Standard repositories are generated via macros to eliminate boilerplate.
//! Custom repositories (OAuth credentials, module settings) are kept as
//! separate modules with hand-written implementations.

pub mod generic_json_repository;
pub mod module_settings_json_repository;
pub mod module_settings_repository;
pub mod oauth_credential_json_repository;
pub mod oauth_credential_repository;
pub mod provider_repository;

// Re-export shared error infrastructure.
pub use provider_repository::{BoxFuture, RepositoryError, RepositoryResult};

// Re-export custom repository types.
pub use module_settings_json_repository::ModuleSettingsJsonRepository;
pub use module_settings_repository::ModuleSettingsRepository;
pub use oauth_credential_json_repository::JsonOAuthCredentialRepository;
pub use oauth_credential_repository::OAuthCredentialRepository;

// ── Macros for generating repository boilerplate ─────────────────────────────

/// Generate a repository trait with `load`/`save` methods and a JSON
/// implementation struct that delegates to [`generic_json_repository::GenericJsonRepository`].
macro_rules! define_single_json_repository {
    (
        trait $TraitName:ident,
        struct $StructName:ident,
        model = $Model:ty,
        filename = $filename:expr $(,)?
    ) => {
        pub trait $TraitName: Send + Sync + 'static {
            fn load(
                &self,
            ) -> provider_repository::BoxFuture<
                'static,
                provider_repository::RepositoryResult<$Model>,
            >;
            fn save(
                &self,
                value: $Model,
            ) -> provider_repository::BoxFuture<'static, provider_repository::RepositoryResult<()>>;
        }

        pub struct $StructName {
            inner: generic_json_repository::GenericJsonRepository<$Model>,
        }

        impl $StructName {
            pub fn new() -> provider_repository::RepositoryResult<Self> {
                Ok(Self {
                    inner: generic_json_repository::GenericJsonRepository::new($filename)?,
                })
            }
        }

        impl $TraitName for $StructName {
            fn load(
                &self,
            ) -> provider_repository::BoxFuture<
                'static,
                provider_repository::RepositoryResult<$Model>,
            > {
                self.inner.load()
            }

            fn save(
                &self,
                value: $Model,
            ) -> provider_repository::BoxFuture<'static, provider_repository::RepositoryResult<()>>
            {
                self.inner.save(value)
            }
        }
    };
}

/// Generate a repository trait with `load_all`/`save_all` methods and a JSON
/// implementation struct that delegates to [`generic_json_repository::GenericJsonListRepository`].
macro_rules! define_list_json_repository {
    (
        trait $TraitName:ident,
        struct $StructName:ident,
        model = $Model:ty,
        filename = $filename:expr $(,)?
    ) => {
        pub trait $TraitName: Send + Sync + 'static {
            fn load_all(
                &self,
            ) -> provider_repository::BoxFuture<
                'static,
                provider_repository::RepositoryResult<Vec<$Model>>,
            >;
            fn save_all(
                &self,
                items: Vec<$Model>,
            ) -> provider_repository::BoxFuture<'static, provider_repository::RepositoryResult<()>>;
        }

        pub struct $StructName {
            inner: generic_json_repository::GenericJsonListRepository<$Model>,
        }

        impl $StructName {
            pub fn new() -> provider_repository::RepositoryResult<Self> {
                Ok(Self {
                    inner: generic_json_repository::GenericJsonListRepository::new($filename)?,
                })
            }
        }

        impl $TraitName for $StructName {
            fn load_all(
                &self,
            ) -> provider_repository::BoxFuture<
                'static,
                provider_repository::RepositoryResult<Vec<$Model>>,
            > {
                self.inner.load_all()
            }

            fn save_all(
                &self,
                items: Vec<$Model>,
            ) -> provider_repository::BoxFuture<'static, provider_repository::RepositoryResult<()>>
            {
                self.inner.save_all(items)
            }
        }
    };
}

// ── Single-object repositories (load/save) ───────────────────────────────────

define_single_json_repository!(
    trait GeneralSettingsRepository,
    struct GeneralSettingsJsonRepository,
    model = crate::settings::models::general_model::GeneralSettingsModel,
    filename = "general_settings.json",
);

define_single_json_repository!(
    trait ExecutionSettingsRepository,
    struct ExecutionSettingsJsonRepository,
    model = crate::settings::models::execution_settings::ExecutionSettingsModel,
    filename = "execution_settings.json",
);

define_single_json_repository!(
    trait SearchSettingsRepository,
    struct SearchSettingsJsonRepository,
    model = crate::settings::models::search_settings::SearchSettingsModel,
    filename = "search_settings.json",
);

define_single_json_repository!(
    trait TrainingSettingsRepository,
    struct TrainingSettingsJsonRepository,
    model = crate::settings::models::training_settings::TrainingSettingsModel,
    filename = "training_settings.json",
);

define_single_json_repository!(
    trait UserSecretsRepository,
    struct UserSecretsJsonRepository,
    model = crate::settings::models::user_secrets_store::UserSecretsModel,
    filename = "user_secrets.json",
);

#[cfg(test)]
impl UserSecretsJsonRepository {
    pub(crate) fn with_path(file_path: std::path::PathBuf) -> Self {
        Self {
            inner: generic_json_repository::GenericJsonRepository::with_path(file_path),
        }
    }
}

define_single_json_repository!(
    trait HiveSettingsRepository,
    struct HiveSettingsJsonRepository,
    model = crate::settings::models::hive_settings::HiveSettingsModel,
    filename = "hive_settings.json",
);

define_single_json_repository!(
    trait ExtensionsRepository,
    struct ExtensionsJsonRepository,
    model = crate::settings::models::extensions_store::ExtensionsModel,
    filename = "extensions.json",
);

// ── List-based repositories (load_all/save_all) ─────────────────────────────

define_list_json_repository!(
    trait ProviderRepository,
    struct JsonFileRepository,
    model = crate::settings::models::providers_store::ProviderConfig,
    filename = "providers.json",
);

define_list_json_repository!(
    trait ModelsRepository,
    struct JsonModelsRepository,
    model = crate::settings::models::models_store::ModelConfig,
    filename = "models.json",
);

define_list_json_repository!(
    trait McpRepository,
    struct JsonMcpRepository,
    model = crate::settings::models::mcp_store::McpServerConfig,
    filename = "mcp_servers.json",
);

define_list_json_repository!(
    trait A2aRepository,
    struct A2aJsonRepository,
    model = crate::settings::models::a2a_store::A2aAgentConfig,
    filename = "a2a_agents.json",
);

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::models::user_secrets_store::{UserSecret, UserSecretsModel};

    #[tokio::test]
    async fn test_user_secrets_save_load_roundtrip() {
        let dir =
            std::env::temp_dir().join(format!("chatty_secrets_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("user_secrets.json");

        let repo = UserSecretsJsonRepository::with_path(path);

        let model = UserSecretsModel {
            secrets: vec![
                UserSecret {
                    key: "KEY_A".into(),
                    value: "value_a".into(),
                },
                UserSecret {
                    key: "KEY_B".into(),
                    value: "val with 'quotes'".into(),
                },
            ],
            ..Default::default()
        };

        repo.save(model.clone()).await.unwrap();
        let loaded = repo.load().await.unwrap();

        assert_eq!(loaded.secrets.len(), 2);
        assert_eq!(loaded.secrets[0].key, "KEY_A");
        assert_eq!(loaded.secrets[0].value, "value_a");
        assert_eq!(loaded.secrets[1].key, "KEY_B");
        assert_eq!(loaded.secrets[1].value, "val with 'quotes'");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
