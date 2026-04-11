use super::provider_repository::{BoxFuture, RepositoryResult};
use crate::settings::models::user_secrets_store::UserSecretsModel;

pub trait UserSecretsRepository: Send + Sync + 'static {
    /// Load user secrets from storage
    fn load(&self) -> BoxFuture<'static, RepositoryResult<UserSecretsModel>>;

    /// Save user secrets to storage
    fn save(&self, secrets: UserSecretsModel) -> BoxFuture<'static, RepositoryResult<()>>;
}
