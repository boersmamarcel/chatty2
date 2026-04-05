use std::pin::Pin;

use crate::settings::models::extensions_store::ExtensionsModel;
use crate::settings::repositories::provider_repository::RepositoryResult;

pub type BoxFuture<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

pub trait ExtensionsRepository: Send + Sync {
    fn load(&self) -> BoxFuture<'static, RepositoryResult<ExtensionsModel>>;
    fn save(&self, model: ExtensionsModel) -> BoxFuture<'static, RepositoryResult<()>>;
}
