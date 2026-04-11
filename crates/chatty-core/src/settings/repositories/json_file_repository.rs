use super::generic_json_repository::GenericJsonListRepository;
use super::provider_repository::{BoxFuture, ProviderRepository, RepositoryResult};
use crate::settings::models::providers_store::ProviderConfig;

pub struct JsonFileRepository {
    inner: GenericJsonListRepository<ProviderConfig>,
}

impl JsonFileRepository {
    /// Create repository with XDG-compliant path
    pub fn new() -> RepositoryResult<Self> {
        Ok(Self {
            inner: GenericJsonListRepository::new("providers.json")?,
        })
    }
}

impl ProviderRepository for JsonFileRepository {
    fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<ProviderConfig>>> {
        self.inner.load_all()
    }

    fn save_all(&self, providers: Vec<ProviderConfig>) -> BoxFuture<'static, RepositoryResult<()>> {
        self.inner.save_all(providers)
    }
}
