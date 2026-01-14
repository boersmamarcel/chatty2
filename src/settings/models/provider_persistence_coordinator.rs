use std::sync::Arc;

use super::persistence_error::ProviderPersistenceError;
use super::provider_repository::ProviderRepository;
use super::providers_store::ProviderConfig;

pub struct ProviderPersistenceCoordinator {
    repository: Arc<dyn ProviderRepository>,
}

impl ProviderPersistenceCoordinator {
    pub fn new(repository: Arc<dyn ProviderRepository>) -> Self {
        Self { repository }
    }

    /// Load providers synchronously (blocks until complete)
    /// Called once at app startup
    pub fn load_providers_blocking(&self) -> Result<Vec<ProviderConfig>, ProviderPersistenceError> {
        let repo = self.repository.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async move { repo.load_all().await })
        })
        .join()
        .unwrap()
    }

    /// Save providers asynchronously with optional rollback callback
    /// Returns immediately - save happens in background
    pub fn save_providers_async<F>(&self, providers: Vec<ProviderConfig>, on_failure: Option<F>)
    where
        F: FnOnce(ProviderPersistenceError) + Send + 'static,
    {
        let repo = self.repository.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async move {
                if let Err(e) = repo.save_all(providers).await {
                    if let Some(callback) = on_failure {
                        callback(e);
                    }
                }
            });
        });
    }
}
