//! Generic JSON file repository implementations.
//!
//! Provides [`GenericJsonRepository`] for single-object settings and
//! [`GenericJsonListRepository`] for collection-based settings, eliminating
//! duplicated load/save boilerplate across the codebase.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use serde::{Serialize, de::DeserializeOwned};

use super::provider_repository::{RepositoryError, RepositoryResult};

/// Resolve the chatty config directory (`$XDG_CONFIG_HOME/chatty`).
fn chatty_config_dir() -> RepositoryResult<PathBuf> {
    let config_dir = dirs::config_dir()
        .ok_or_else(|| RepositoryError::PathError("Cannot determine config directory".into()))?;
    Ok(config_dir.join("chatty"))
}

// ── Ordered atomic writes ────────────────────────────────────────────────────

/// Sequence of `save` calls, process-wide. Taken synchronously when `save` is
/// *called*, so it records call order even when the returned futures are
/// polled out of order.
static NEXT_SAVE: AtomicU64 = AtomicU64::new(1);

/// Per file: the sequence number of the last save written, behind an async
/// lock held across write + rename, so saves of one file never interleave.
type WriteSlot = Arc<tokio::sync::Mutex<u64>>;
static WRITE_SLOTS: LazyLock<Mutex<HashMap<PathBuf, WriteSlot>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// A save's place in line for its file (AGE-562).
struct SaveTicket {
    seq: u64,
    slot: WriteSlot,
}

impl SaveTicket {
    fn take(path: &Path) -> Self {
        let seq = NEXT_SAVE.fetch_add(1, Ordering::Relaxed);
        let slot = WRITE_SLOTS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(path.to_path_buf())
            .or_default()
            .clone();
        Self { seq, slot }
    }
}

/// Write `json` to `path` atomically (temp file + rename), newest call wins.
///
/// Settings are saved on every change (a text field saves per keystroke), so
/// two saves of one file routinely overlap. They used to share one temp path,
/// and the loser of the race failed its rename with `ENOENT` — or an older
/// value was renamed over a newer one. Saves of a file are now serialized,
/// each has its own temp file, and a save older than the last one written is
/// dropped rather than written over it.
async fn write_ordered(path: &Path, json: String, ticket: SaveTicket) -> RepositoryResult<()> {
    let mut last_written = ticket.slot.lock().await;
    if *last_written > ticket.seq {
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| RepositoryError::IoError(e.to_string()))?;
    }

    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp_path = path.with_extension(format!(
        "{extension}.{}.{}.tmp",
        std::process::id(),
        ticket.seq
    ));
    tokio::fs::write(&temp_path, &json)
        .await
        .map_err(|e| RepositoryError::IoError(e.to_string()))?;

    if let Err(e) = tokio::fs::rename(&temp_path, path).await {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(RepositoryError::IoError(e.to_string()));
    }

    *last_written = ticket.seq;
    Ok(())
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

    /// Create a repository with a custom file path (useful for testing, and
    /// for the `store_conformance` suite exported behind `test-support`).
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_path(file_path: PathBuf) -> Self {
        Self {
            file_path,
            _marker: std::marker::PhantomData,
        }
    }

    /// Returns a reference to the underlying file path.
    pub fn file_path(&self) -> &std::path::Path {
        &self.file_path
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
    pub fn save(
        &self,
        value: T,
    ) -> super::provider_repository::BoxFuture<'static, RepositoryResult<()>> {
        let path = self.file_path.clone();
        let ticket = SaveTicket::take(&path);

        Box::pin(async move {
            let json = serde_json::to_string_pretty(&value)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            write_ordered(&path, json, ticket).await
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

    /// Create a repository with a custom file path (useful for testing, and
    /// for the `store_conformance` suite exported behind `test-support`).
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_path(file_path: PathBuf) -> Self {
        Self {
            file_path,
            _marker: std::marker::PhantomData,
        }
    }

    /// Load all items from disk, returning an empty `Vec` if the file is missing.
    pub fn load_all(
        &self,
    ) -> super::provider_repository::BoxFuture<'static, RepositoryResult<Vec<T>>> {
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
    pub fn save_all(
        &self,
        items: Vec<T>,
    ) -> super::provider_repository::BoxFuture<'static, RepositoryResult<()>> {
        let path = self.file_path.clone();
        let ticket = SaveTicket::take(&path);

        Box::pin(async move {
            let json = serde_json::to_string_pretty(&items)
                .map_err(|e| RepositoryError::SerializationError(e.to_string()))?;

            write_ordered(&path, json, ticket).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Overlapping saves of one file all succeed and the last *call* wins,
    /// even when the futures are polled in reverse (AGE-562).
    #[tokio::test]
    async fn concurrent_saves_all_succeed_and_last_call_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("general_settings.json");
        let repo = GenericJsonRepository::<u32>::with_path(path.clone());

        let saves: Vec<_> = (0..50u32).map(|v| repo.save(v)).collect();
        let results = futures::future::join_all(saves.into_iter().rev()).await;

        assert!(results.iter().all(|r| r.is_ok()), "{results:?}");
        assert_eq!(repo.load().await.unwrap(), 49);
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n != "general_settings.json")
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
    }

    #[tokio::test]
    async fn concurrent_list_saves_all_succeed_and_last_call_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.json");
        let repo = GenericJsonListRepository::<u32>::with_path(path);

        let saves: Vec<_> = (0..50u32).map(|v| repo.save_all(vec![v, v])).collect();
        let results = futures::future::join_all(saves).await;

        assert!(results.iter().all(|r| r.is_ok()), "{results:?}");
        assert_eq!(repo.load_all().await.unwrap(), vec![49, 49]);
    }
}
