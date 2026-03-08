use thiserror::Error;

#[derive(Debug, Error)]
#[allow(clippy::enum_variant_names)]
pub enum RepositoryError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("Repository initialization failed: {message}")]
    InitializationError { message: String },

    #[error("Database error: {0}")]
    DatabaseError(#[from] sqlx::Error),
}

pub type RepositoryResult<T> = Result<T, RepositoryError>;
