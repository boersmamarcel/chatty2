use std::fmt;
use std::future::Future;
use std::pin::Pin;

use crate::settings::models::providers_store::ProviderConfig;

/// Repository error type - abstracts over specific implementation errors
#[derive(Debug)]
pub enum RepositoryError {
    IoError(String),
    SerializationError(String),
    PathError(String),
}

impl fmt::Display for RepositoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IoError(msg) => write!(f, "I/O error: {}", msg),
            Self::SerializationError(msg) => write!(f, "Serialization error: {}", msg),
            Self::PathError(msg) => write!(f, "Path error: {}", msg),
        }
    }
}

impl std::error::Error for RepositoryError {}

pub type RepositoryResult<T> = Result<T, RepositoryError>;
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait ProviderRepository: Send + Sync + 'static {
    /// Load all provider configurations from storage
    fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<ProviderConfig>>>;

    /// Save all provider configurations to storage
    fn save_all(&self, providers: Vec<ProviderConfig>) -> BoxFuture<'static, RepositoryResult<()>>;

    /// Get the storage path (for diagnostics)
    fn storage_path(&self) -> String;
}
