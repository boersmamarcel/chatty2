use std::future::Future;
use std::pin::Pin;

use super::provider_repository::RepositoryResult;
use crate::settings::models::user_secrets_store::UserSecretsModel;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait UserSecretsRepository: Send + Sync + 'static {
    /// Load user secrets from storage
    fn load(&self) -> BoxFuture<'static, RepositoryResult<UserSecretsModel>>;

    /// Save user secrets to storage
    fn save(&self, secrets: UserSecretsModel) -> BoxFuture<'static, RepositoryResult<()>>;
}
