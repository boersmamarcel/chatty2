use std::future::Future;
use std::pin::Pin;

use super::provider_repository::RepositoryResult;
use crate::settings::models::browser_credentials_store::BrowserCredentialsModel;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait BrowserCredentialsRepository: Send + Sync + 'static {
    /// Load browser credentials from storage.
    fn load(&self) -> BoxFuture<'static, RepositoryResult<BrowserCredentialsModel>>;

    /// Save browser credentials to storage.
    fn save(
        &self,
        credentials: BrowserCredentialsModel,
    ) -> BoxFuture<'static, RepositoryResult<()>>;
}
