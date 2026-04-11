use std::fmt;
use std::future::Future;
use std::pin::Pin;

/// Repository error type - abstracts over specific implementation errors
#[derive(Debug)]
#[allow(clippy::enum_variant_names)]
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
