use std::future::Future;
use std::pin::Pin;

use super::persistence_error::ProviderPersistenceError;
use super::providers_store::ProviderConfig;

pub type RepositoryResult<T> = Result<T, ProviderPersistenceError>;
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait ProviderRepository: Send + Sync + 'static {
    /// Load all provider configurations from storage
    fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<ProviderConfig>>>;

    /// Save all provider configurations to storage
    fn save_all(&self, providers: Vec<ProviderConfig>) -> BoxFuture<'static, RepositoryResult<()>>;

    /// Get the storage path (for diagnostics)
    fn storage_path(&self) -> String;
}
