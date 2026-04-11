use crate::settings::models::extensions_store::ExtensionsModel;
use crate::settings::repositories::provider_repository::{BoxFuture, RepositoryResult};

pub trait ExtensionsRepository: Send + Sync {
    fn load(&self) -> BoxFuture<'static, RepositoryResult<ExtensionsModel>>;
    fn save(&self, model: ExtensionsModel) -> BoxFuture<'static, RepositoryResult<()>>;
}
