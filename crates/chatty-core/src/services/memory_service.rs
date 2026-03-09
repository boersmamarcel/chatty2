use anyhow::{Context, Result};
use memvid_core::{Memvid, PutOptions, SearchRequest};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

/// Maximum number of search results returned by default
const DEFAULT_TOP_K: usize = 5;

/// Snippet length for search results
const SNIPPET_CHARS: usize = 500;

/// A single memory search result
#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryHit {
    pub text: String,
    pub title: Option<String>,
    pub score: f32,
}

/// Statistics about the memory store
#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryStats {
    pub entry_count: usize,
    pub file_size_bytes: u64,
}

/// Service wrapping memvid-core for persistent agent memory.
///
/// All operations are async-safe via a Tokio mutex. The underlying `.mv2` file
/// is created lazily on first write.
#[derive(Clone)]
pub struct MemoryService {
    memvid: Arc<Mutex<Memvid>>,
    path: PathBuf,
}

impl MemoryService {
    /// Open an existing memory file or create a new one.
    ///
    /// The file is stored at `{data_dir}/memory.mv2`.
    pub async fn open_or_create(data_dir: &Path) -> Result<Self> {
        tokio::fs::create_dir_all(data_dir)
            .await
            .context("Failed to create memory data directory")?;

        let path = data_dir.join("memory.mv2");

        let memvid = if path.exists() {
            info!(path = %path.display(), "Opening existing memory file");
            Memvid::open(&path).context("Failed to open memory file")?
        } else {
            info!(path = %path.display(), "Creating new memory file");
            Memvid::create(&path).context("Failed to create memory file")?
        };

        Ok(Self {
            memvid: Arc::new(Mutex::new(memvid)),
            path,
        })
    }

    /// Store a memory entry with an optional title and key-value tags.
    pub async fn remember(
        &self,
        content: &str,
        title: Option<&str>,
        tags: &[(&str, &str)],
    ) -> Result<()> {
        let mut mem = self.memvid.lock().await;

        let mut builder = PutOptions::builder();
        if let Some(t) = title {
            builder = builder.title(t);
        }
        for &(key, value) in tags {
            builder = builder.tag(key, value);
        }
        let opts = builder.build();

        mem.put_bytes_with_options(content.as_bytes(), opts)
            .context("Failed to store memory")?;
        mem.commit().context("Failed to commit memory")?;

        info!(
            title = title.unwrap_or("<untitled>"),
            content_len = content.len(),
            "Stored new memory"
        );

        Ok(())
    }

    /// Search memory by natural language query.
    ///
    /// Returns empty results gracefully if the memory store is empty or
    /// if the search engine errors on an empty index.
    pub async fn search(&self, query: &str, top_k: Option<usize>) -> Result<Vec<MemoryHit>> {
        let mut mem = self.memvid.lock().await;

        let request = make_search_request(query, top_k.unwrap_or(DEFAULT_TOP_K), SNIPPET_CHARS);

        let response = match mem.search(request) {
            Ok(resp) => resp,
            Err(e) => {
                // memvid-core may error when searching an empty store;
                // treat this as "no results" rather than a hard failure.
                warn!(error = ?e, "Memory search returned error, treating as empty");
                return Ok(Vec::new());
            }
        };

        let hits = response
            .hits
            .into_iter()
            .map(|hit| MemoryHit {
                text: hit.text,
                title: hit.title,
                score: hit.score.unwrap_or(0.0),
            })
            .collect();

        Ok(hits)
    }

    /// Get statistics about the memory store.
    pub async fn stats(&self) -> Result<MemoryStats> {
        let metadata = tokio::fs::metadata(&self.path).await;
        let file_size_bytes = metadata.map(|m| m.len()).unwrap_or(0);

        let mut mem = self.memvid.lock().await;
        let request = make_search_request("", 0, 0);
        let entry_count = mem.search(request).map(|r| r.total_hits).unwrap_or(0);

        Ok(MemoryStats {
            entry_count,
            file_size_bytes,
        })
    }

    /// Clear all memory by replacing the file with a fresh one.
    pub async fn clear(&self) -> Result<()> {
        let mut mem = self.memvid.lock().await;

        warn!(path = %self.path.display(), "Clearing all agent memory");

        // Drop old and create fresh
        *mem = Memvid::create(&self.path).context("Failed to recreate memory file")?;

        Ok(())
    }

    /// The path to the memory file on disk.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Build a SearchRequest with sensible defaults for optional fields.
fn make_search_request(query: &str, top_k: usize, snippet_chars: usize) -> SearchRequest {
    SearchRequest {
        query: query.to_string(),
        top_k,
        snippet_chars,
        uri: None,
        scope: None,
        cursor: None,
        as_of_frame: None,
        as_of_ts: None,
        no_sketch: false,
        acl_context: None,
        acl_enforcement_mode: Default::default(),
    }
}

/// Returns the platform-specific data directory for memory storage.
pub fn memory_data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("chatty"))
}
