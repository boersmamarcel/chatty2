use thiserror::Error;

#[derive(Debug, Error)]
#[allow(dead_code)]
pub enum RepositoryError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("Conversation not found: {id}")]
    NotFound { id: String },

    #[error("Invalid data format: {message}")]
    InvalidData { message: String },

    #[error("Repository initialization failed: {message}")]
    InitializationError { message: String },
}

pub type RepositoryResult<T> = Result<T, RepositoryError>;
