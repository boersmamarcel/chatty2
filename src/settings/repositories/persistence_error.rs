use std::fmt;

#[derive(Debug)]
pub enum ProviderPersistenceError {
    IoError(std::io::Error),
    SerializationError(serde_json::Error),
    InvalidData(String),
    PathError(String),
}

impl fmt::Display for ProviderPersistenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IoError(e) => write!(f, "I/O error: {}", e),
            Self::SerializationError(e) => write!(f, "Serialization error: {}", e),
            Self::InvalidData(msg) => write!(f, "Invalid data: {}", msg),
            Self::PathError(msg) => write!(f, "Path error: {}", msg),
        }
    }
}

impl std::error::Error for ProviderPersistenceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::IoError(e) => Some(e),
            Self::SerializationError(e) => Some(e),
            _ => None,
        }
    }
}
