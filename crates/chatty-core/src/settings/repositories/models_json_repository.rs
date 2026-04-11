use super::generic_json_repository::GenericJsonListRepository;
use super::models_repository::ModelsRepository;
use super::provider_repository::{BoxFuture, RepositoryResult};
use crate::settings::models::models_store::ModelConfig;

pub struct JsonModelsRepository {
    inner: GenericJsonListRepository<ModelConfig>,
}

impl JsonModelsRepository {
    /// Create repository with XDG-compliant path
    pub fn new() -> RepositoryResult<Self> {
        Ok(Self {
            inner: GenericJsonListRepository::new("models.json")?,
        })
    }
}

impl ModelsRepository for JsonModelsRepository {
    fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<ModelConfig>>> {
        self.inner.load_all()
    }

    fn save_all(&self, models: Vec<ModelConfig>) -> BoxFuture<'static, RepositoryResult<()>> {
        self.inner.save_all(models)
    }
}
