use super::extensions_repository::ExtensionsRepository;
use super::generic_json_repository::GenericJsonRepository;
use super::provider_repository::{BoxFuture, RepositoryResult};
use crate::settings::models::extensions_store::ExtensionsModel;

pub struct ExtensionsJsonRepository {
    inner: GenericJsonRepository<ExtensionsModel>,
}

impl ExtensionsJsonRepository {
    pub fn new() -> RepositoryResult<Self> {
        Ok(Self {
            inner: GenericJsonRepository::new("extensions.json")?,
        })
    }
}

impl ExtensionsRepository for ExtensionsJsonRepository {
    fn load(&self) -> BoxFuture<'static, RepositoryResult<ExtensionsModel>> {
        self.inner.load()
    }

    fn save(&self, model: ExtensionsModel) -> BoxFuture<'static, RepositoryResult<()>> {
        self.inner.save(model)
    }
}
