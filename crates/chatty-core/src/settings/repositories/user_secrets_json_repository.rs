use super::generic_json_repository::GenericJsonRepository;
use super::provider_repository::{BoxFuture, RepositoryResult};
use super::user_secrets_repository::UserSecretsRepository;
use crate::settings::models::user_secrets_store::UserSecretsModel;

pub struct UserSecretsJsonRepository {
    inner: GenericJsonRepository<UserSecretsModel>,
}

impl UserSecretsJsonRepository {
    /// Create repository with XDG-compliant path
    pub fn new() -> RepositoryResult<Self> {
        Ok(Self {
            inner: GenericJsonRepository::new("user_secrets.json")?,
        })
    }

    /// Create repository with a custom file path (for testing)
    #[cfg(test)]
    pub(crate) fn with_path(file_path: std::path::PathBuf) -> Self {
        Self {
            inner: GenericJsonRepository::with_path(file_path),
        }
    }
}

impl UserSecretsRepository for UserSecretsJsonRepository {
    fn load(&self) -> BoxFuture<'static, RepositoryResult<UserSecretsModel>> {
        self.inner.load()
    }

    fn save(&self, secrets: UserSecretsModel) -> BoxFuture<'static, RepositoryResult<()>> {
        self.inner.save(secrets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::models::user_secrets_store::{UserSecret, UserSecretsModel};
    use crate::settings::repositories::user_secrets_repository::UserSecretsRepository;

    #[tokio::test]
    async fn test_repository_save_load_roundtrip() {
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
