//! Generic JSON file repository implementations.
//!
//! Provides [`GenericJsonRepository`] for single-object settings and
//! [`GenericJsonListRepository`] for collection-based settings, eliminating
//! duplicated load/save boilerplate across the codebase.

use std::path::PathBuf;

use serde::{Serialize, de::DeserializeOwned};

use super::provider_repository::{RepositoryError, RepositoryResult};

/// Resolve the chatty config directory (`$XDG_CONFIG_HOME/chatty`).
fn chatty_config_dir() -> RepositoryResult<PathBuf> {
    let config_dir = dirs::config_dir()
        .ok_or_else(|| RepositoryError::PathError("Cannot determine config directory".into()))?;
    Ok(config_dir.join("chatty"))
}

// ── Single-object repository ─────────────────────────────────────────────────

/// Generic JSON repository for a **single settings object** (`load` / `save`).
///
/// `T` must be `Serialize + DeserializeOwned + Default + Send + 'static`.
pub struct GenericJsonRepository<T> {
    file_path: PathBuf,
    _marker: std::marker::PhantomData<T>,
}

impl<T> GenericJsonRepository<T>
where
    T: Serialize + DeserializeOwned + Default + Send + 'static,
{
    /// Create a repository that persists to `<config_dir>/chatty/<filename>`.
    pub fn new(filename: &str) -> RepositoryResult<Self> {
        let file_path = chatty_config_dir()?.join(filename);
        Ok(Self {
            file_path,
            _marker: std::marker::PhantomData,
        })
    }

    /// Create a repository with a custom file path (useful for testing).
    #[cfg(test)]
    pub fn with_path(file_path: PathBuf) -> Self {
        Self {
            file_path,
            _marker: std::marker::PhantomData,
        }
    }

    /// Load the settings from disk, returning `T::default()` if the file is missing.
    pub fn load(&self) -> super::provider_repository::BoxFuture<'static, RepositoryResult<T>> {
        let path = self.file_path.clone();

        Box::pin(async move {
            if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
                return Ok(T::default());
            }

            let contents = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            let value: T = serde_json::from_str(&contents)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            Ok(value)
        })
    }

    /// Save the settings to disk atomically (temp file + rename).
    pub fn save(&self, value: T) -> super::provider_repository::BoxFuture<'static, RepositoryResult<()>> {
        let path = self.file_path.clone();

        Box::pin(async move {
            let json = serde_json::to_string_pretty(&value)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| RepositoryError::IoError(e.to_string()))?;
            }

            let temp_path = path.with_extension(format!("json.{}.tmp", std::process::id()));
            tokio::fs::write(&temp_path, &json)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            tokio::fs::rename(&temp_path, &path)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            Ok(())
        })
    }
}

// ── Collection repository ────────────────────────────────────────────────────

/// Generic JSON repository for a **collection of items** (`load_all` / `save_all`).
///
/// `T` must be `Serialize + DeserializeOwned + Send + 'static`.
pub struct GenericJsonListRepository<T> {
    file_path: PathBuf,
    _marker: std::marker::PhantomData<T>,
}

impl<T> GenericJsonListRepository<T>
where
    T: Serialize + DeserializeOwned + Send + 'static,
{
    /// Create a repository that persists to `<config_dir>/chatty/<filename>`.
    pub fn new(filename: &str) -> RepositoryResult<Self> {
        let file_path = chatty_config_dir()?.join(filename);
        Ok(Self {
            file_path,
            _marker: std::marker::PhantomData,
        })
    }

    /// Load all items from disk, returning an empty `Vec` if the file is missing.
    pub fn load_all(&self) -> super::provider_repository::BoxFuture<'static, RepositoryResult<Vec<T>>> {
        let path = self.file_path.clone();

        Box::pin(async move {
            if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
                return Ok(Vec::new());
            }

            let contents = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            let items: Vec<T> = serde_json::from_str(&contents)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            Ok(items)
        })
    }

    /// Save all items to disk atomically (temp file + rename).
    pub fn save_all(&self, items: Vec<T>) -> super::provider_repository::BoxFuture<'static, RepositoryResult<()>> {
        let path = self.file_path.clone();

        Box::pin(async move {
            let json = serde_json::to_string_pretty(&items)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| RepositoryError::IoError(e.to_string()))?;
            }

            let temp_path = path.with_extension(format!("json.{}.tmp", std::process::id()));
            tokio::fs::write(&temp_path, &json)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            tokio::fs::rename(&temp_path, &path)
                .await
                .map_err(|e| RepositoryError::IoError(e.to_string()))?;

            Ok(())
        })
    }
}
