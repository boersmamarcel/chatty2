//! Disk-backed cache for module listings (offline support).

use std::path::{Path, PathBuf};

use crate::models::ModuleList;

/// Simple file-system cache for module listings.
///
/// Each entry is stored as `<dir>/<key>.json`.
pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    /// Create a new cache backed by `dir`. The directory is created if needed.
    pub fn new(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// Write `list` to `<dir>/<key>.json`, overwriting any previous value.
    ///
    /// Uses synchronous `std::fs::write` intentionally — cached payloads are
    /// small JSON blobs (typically < 50 KB) so the blocking cost is negligible.
    /// Converting to `tokio::fs` would require making the `Cache` API async,
    /// which adds complexity without meaningful benefit.
    pub fn store(&self, key: &str, list: &ModuleList) -> std::io::Result<()> {
        let path = self.entry_path(key);
        let json = serde_json::to_vec(list)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&path, &json)?;
        tracing::debug!(cache_key = %key, path = %path.display(), "module list cached");
        Ok(())
    }

    /// Read a previously stored module list. Returns `None` if missing or corrupt.
    pub fn load(&self, key: &str) -> Option<ModuleList> {
        let path = self.entry_path(key);
        let data = std::fs::read(&path).ok()?;
        match serde_json::from_slice::<ModuleList>(&data) {
            Ok(list) => {
                tracing::debug!(cache_key = %key, "serving cached module list");
                Some(list)
            }
            Err(e) => {
                tracing::warn!(cache_key = %key, error = %e, "failed to parse cached module list");
                None
            }
        }
    }

    fn entry_path(&self, key: &str) -> PathBuf {
        let safe: String = key
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        Path::join(&self.dir, format!("{safe}.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AuthorMetadata, ModuleList, ModuleMetadata};
    use chrono::Utc;
    use uuid::Uuid;

    fn dummy_list() -> ModuleList {
        ModuleList {
            items: vec![ModuleMetadata {
                name: "test-module".to_string(),
                display_name: "Test".to_string(),
                description: "A test module".to_string(),
                author: AuthorMetadata {
                    id: Uuid::nil(),
                    username: "alice".to_string(),
                },
                latest_version: Some("1.0.0".to_string()),
                license: Some("MIT".to_string()),
                tags: vec!["testing".to_string()],
                category: None,
                downloads: 42,
                pricing_model: "free".to_string(),
                execution_mode: "local".to_string(),
                homepage: None,
                support_email: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            }],
            page: 1,
            per_page: 20,
            total: 1,
        }
    }

    #[test]
    fn roundtrip_store_and_load() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = Cache::new(tmp.path()).unwrap();
        let list = dummy_list();

        cache.store("modules", &list).unwrap();
        let loaded = cache.load("modules").expect("cache entry should exist");
        assert_eq!(loaded.items.len(), 1);
        assert_eq!(loaded.items[0].name, "test-module");
    }

    #[test]
    fn load_missing_key_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = Cache::new(tmp.path()).unwrap();
        assert!(cache.load("nonexistent").is_none());
    }

    #[test]
    fn key_sanitisation_uses_underscores() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = Cache::new(tmp.path()).unwrap();
        let list = dummy_list();
        cache.store("search/../malicious", &list).unwrap();
        let loaded = cache.load("search/../malicious");
        assert!(loaded.is_some());
    }
}
