//! On-disk LLM response cache for repeatable walking-skeleton runs (AGE-22).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

use crate::OptimizeError;

/// Cache key — two runs with the same key must hit the same entry on a warm cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheKey {
    pub seed: u64,
    pub model_id: String,
    pub preamble: String,
    pub question_id: String,
}

/// Naive JSON file cache under a directory (one file per key hash).
#[derive(Debug, Clone)]
pub struct ResponseCache {
    dir: PathBuf,
}

impl ResponseCache {
    pub fn new(dir: impl AsRef<Path>) -> Result<Self, OptimizeError> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)
            .map_err(|e| OptimizeError::InvalidInput(format!("cache dir create failed: {e}")))?;
        Ok(Self { dir })
    }

    fn path_for(&self, key: &CacheKey) -> PathBuf {
        let mut hasher = Sha256::new();
        hasher.update(key.seed.to_le_bytes());
        hasher.update(key.model_id.as_bytes());
        hasher.update(key.preamble.as_bytes());
        hasher.update(key.question_id.as_bytes());
        let digest = hex::encode(hasher.finalize());
        self.dir.join(format!("{digest}.json"))
    }

    pub fn get(&self, key: &CacheKey) -> Result<Option<String>, OptimizeError> {
        let path = self.path_for(key);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path)
            .map_err(|e| OptimizeError::InvalidInput(format!("cache read failed: {e}")))?;
        let entry: CacheEntry = serde_json::from_slice(&bytes)
            .map_err(|e| OptimizeError::InvalidInput(format!("cache decode failed: {e}")))?;
        Ok(Some(entry.response))
    }

    pub fn put(&self, key: &CacheKey, response: &str) -> Result<(), OptimizeError> {
        let path = self.path_for(key);
        let entry = CacheEntry {
            key: key.clone(),
            response: response.to_string(),
        };
        let bytes = serde_json::to_vec_pretty(&entry)
            .map_err(|e| OptimizeError::InvalidInput(format!("cache encode failed: {e}")))?;
        fs::write(&path, bytes)
            .map_err(|e| OptimizeError::InvalidInput(format!("cache write failed: {e}")))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    key: CacheKey,
    response: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn put_then_get_roundtrip() {
        let dir = tempdir().unwrap();
        let cache = ResponseCache::new(dir.path()).unwrap();
        let key = CacheKey {
            seed: 42,
            model_id: "test-model".into(),
            preamble: "You are helpful.".into(),
            question_id: "hp-1".into(),
        };
        assert!(cache.get(&key).unwrap().is_none());
        cache.put(&key, "Paris").unwrap();
        assert_eq!(cache.get(&key).unwrap().as_deref(), Some("Paris"));
    }

    #[test]
    fn same_key_same_path_deterministic() {
        let dir = tempdir().unwrap();
        let cache = ResponseCache::new(dir.path()).unwrap();
        let key = CacheKey {
            seed: 7,
            model_id: "m".into(),
            preamble: "p".into(),
            question_id: "q".into(),
        };
        let p1 = cache.path_for(&key);
        let p2 = cache.path_for(&key);
        assert_eq!(p1, p2);
    }
}
