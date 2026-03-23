use std::future::Future;
use std::pin::Pin;

use super::provider_repository::RepositoryResult;
use chatty_browser::settings::BrowserSettingsModel;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait BrowserSettingsRepository: Send + Sync + 'static {
    /// Load browser settings from storage
    fn load(&self) -> BoxFuture<'static, RepositoryResult<BrowserSettingsModel>>;

    /// Save browser settings to storage
    fn save(&self, settings: BrowserSettingsModel) -> BoxFuture<'static, RepositoryResult<()>>;
}
