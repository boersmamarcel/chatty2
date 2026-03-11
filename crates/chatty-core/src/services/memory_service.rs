use anyhow::{Context, Result};
use memvid_core::{Memvid, PutOptions, SearchRequest};
use std::path::{Path, PathBuf};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

/// Maximum number of search results returned by default
const DEFAULT_TOP_K: usize = 5;

/// Snippet length for search results
const SNIPPET_CHARS: usize = 500;

/// A single memory search result
#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryHit {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "relevance_score")]
    pub score: f32,
}

/// Statistics about the memory store
#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryStats {
    pub entry_count: usize,
    pub file_size_bytes: u64,
}

/// Commands sent to the dedicated memory thread.
enum MemoryCommand {
    Search {
        query: String,
        top_k: usize,
        reply: oneshot::Sender<Result<Vec<MemoryHit>>>,
    },
    Remember {
        content: String,
        title: Option<String>,
        tags: Vec<(String, String)>,
        reply: oneshot::Sender<Result<()>>,
    },
    Clear {
        reply: oneshot::Sender<Result<()>>,
    },
    Stats {
        reply: oneshot::Sender<Result<MemoryStats>>,
    },
}

/// Service wrapping memvid-core for persistent agent memory.
///
/// Internally owns a dedicated OS thread that keeps all Memvid operations
/// (open, enable_lex, search, put, commit) on a single thread. This avoids
/// blocking the async executor at startup and prevents `LexNotEnabled` errors
/// caused by tantivy's thread-sensitive index state in Tokio's multi-threaded
/// runtime.
///
/// The thread exits when all `MemoryService` clones are dropped (channel closes).
#[derive(Clone)]
pub struct MemoryService {
    cmd_tx: mpsc::Sender<MemoryCommand>,
    path: PathBuf,
}

impl MemoryService {
    /// Open an existing memory file or create a new one.
    ///
    /// The file is stored at `{data_dir}/memory.mv2`.
    ///
    /// Spawns a dedicated OS thread for all Memvid operations. The async
    /// executor is never blocked — `open_or_create` yields while the thread
    /// performs file I/O and index setup.
    pub async fn open_or_create(data_dir: &Path) -> Result<Self> {
        tokio::fs::create_dir_all(data_dir)
            .await
            .context("Failed to create memory data directory")?;

        let path = data_dir.join("memory.mv2");

        let (cmd_tx, cmd_rx) = mpsc::channel::<MemoryCommand>(32);
        let (init_tx, init_rx) = oneshot::channel::<Result<()>>();

        let thread_path = path.clone();

        std::thread::Builder::new()
            .name("memory-service".into())
            .spawn(move || {
                run_memory_thread(thread_path, cmd_rx, init_tx);
            })
            .context("Failed to spawn memory thread")?;

        // Await init (non-blocking: yields to executor while the thread works)
        init_rx
            .await
            .context("Memory thread panicked during init")?
            .context("Memory initialization failed")?;

        Ok(Self { cmd_tx, path })
    }

    /// Store a memory entry with an optional title and key-value tags.
    pub async fn remember(
        &self,
        content: &str,
        title: Option<&str>,
        tags: &[(&str, &str)],
    ) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(MemoryCommand::Remember {
                content: content.to_string(),
                title: title.map(|s| s.to_string()),
                tags: tags
                    .iter()
                    .map(|&(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("Memory thread stopped"))?;

        reply_rx
            .await
            .context("Memory thread dropped reply channel")?
    }

    /// Search memory by natural language query.
    ///
    /// Always delegates to the memory thread. Returns empty results gracefully
    /// if the store is empty or the search engine errors on an empty index.
    pub async fn search(&self, query: &str, top_k: Option<usize>) -> Result<Vec<MemoryHit>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(MemoryCommand::Search {
                query: query.to_string(),
                top_k: top_k.unwrap_or(DEFAULT_TOP_K),
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("Memory thread stopped"))?;

        reply_rx
            .await
            .context("Memory thread dropped reply channel")?
    }

    /// Get statistics about the memory store.
    pub async fn stats(&self) -> Result<MemoryStats> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(MemoryCommand::Stats { reply: reply_tx })
            .await
            .map_err(|_| anyhow::anyhow!("Memory thread stopped"))?;

        reply_rx
            .await
            .context("Memory thread dropped reply channel")?
    }

    /// Clear all memory by replacing the file with a fresh one.
    pub async fn clear(&self) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(MemoryCommand::Clear { reply: reply_tx })
            .await
            .map_err(|_| anyhow::anyhow!("Memory thread stopped"))?;

        reply_rx
            .await
            .context("Memory thread dropped reply channel")?
    }

    /// The path to the memory file on disk.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Runs on the dedicated memory thread. All Memvid operations happen here,
/// ensuring tantivy's lexical index state is always on the same OS thread.
fn run_memory_thread(
    path: PathBuf,
    mut cmd_rx: mpsc::Receiver<MemoryCommand>,
    init_tx: oneshot::Sender<Result<()>>,
) {
    // Phase 1: Open/create + enable lex — all on this thread
    let mut memvid = match init_memvid(&path) {
        Ok(m) => {
            let _ = init_tx.send(Ok(()));
            m
        }
        Err(e) => {
            let _ = init_tx.send(Err(e));
            return;
        }
    };

    // Phase 2: Process commands until all senders are dropped
    while let Some(cmd) = cmd_rx.blocking_recv() {
        match cmd {
            MemoryCommand::Search {
                query,
                top_k,
                reply,
            } => {
                let result = handle_search(&mut memvid, &query, top_k);
                let _ = reply.send(result);
            }
            MemoryCommand::Remember {
                content,
                title,
                tags,
                reply,
            } => {
                let result = handle_remember(&mut memvid, &content, title.as_deref(), &tags);
                let _ = reply.send(result);
            }
            MemoryCommand::Clear { reply } => {
                let result = handle_clear(&mut memvid, &path);
                let _ = reply.send(result);
            }
            MemoryCommand::Stats { reply } => {
                let result = handle_stats(&mut memvid, &path);
                let _ = reply.send(result);
            }
        }
    }

    info!("Memory thread shutting down (all senders dropped)");
}

/// Open or create the Memvid file and enable lexical search.
///
/// After opening an existing file, runs a health check to detect a corrupted
/// lex index. If the index is broken (e.g. empty manifest from a previous bug),
/// recreates the file to restore working lex search. This is a one-time
/// self-healing step — data stored with a broken lex index is unrecoverable.
fn init_memvid(path: &Path) -> Result<Memvid> {
    let existed = path.exists();
    let mut memvid = if existed {
        info!(path = %path.display(), "Opening existing memory file");
        Memvid::open(path).context("Failed to open memory file")?
    } else {
        info!(path = %path.display(), "Creating new memory file");
        Memvid::create(path).context("Failed to create memory file")?
    };

    memvid
        .enable_lex()
        .context("Failed to enable lexical search on memory file")?;

    // Health check: verify lex search actually works on existing files.
    // A previous bug could have corrupted the lex manifest (bytes_length=0),
    // making all data unsearchable. Detect and auto-recover.
    if existed {
        let probe = SearchRequest {
            query: "health_check".to_string(),
            top_k: 1,
            snippet_chars: 0,
            uri: None,
            scope: None,
            cursor: None,
            as_of_frame: None,
            as_of_ts: None,
            no_sketch: false,
            acl_context: None,
            acl_enforcement_mode: Default::default(),
        };
        if let Err(e) = memvid.search(probe) {
            let err_msg = format!("{e}");
            if err_msg.contains("LexNotEnabled") || err_msg.contains("lex") {
                warn!(
                    path = %path.display(),
                    error = %err_msg,
                    "Lex index is corrupted — recreating memory file to restore search"
                );
                let mut fresh = Memvid::create(path).context("Failed to recreate memory file")?;
                fresh
                    .enable_lex()
                    .context("Failed to enable lex on recreated file")?;
                return Ok(fresh);
            }
        }
    }

    Ok(memvid)
}

fn handle_search(memvid: &mut Memvid, query: &str, top_k: usize) -> Result<Vec<MemoryHit>> {
    let request = make_search_request(query, top_k, SNIPPET_CHARS);

    let response = match memvid.search(request) {
        Ok(resp) => resp,
        Err(e) => {
            warn!(error = ?e, query = %query, "Memory search returned error, treating as empty");
            return Ok(Vec::new());
        }
    };

    let hits: Vec<MemoryHit> = response
        .hits
        .into_iter()
        .map(|hit| MemoryHit {
            text: hit.text,
            title: hit.title,
            score: hit.score.unwrap_or(0.0),
        })
        .collect();

    info!(
        query = %query,
        top_k = top_k,
        hit_count = hits.len(),
        total_hits = response.total_hits,
        "Memory search completed"
    );

    Ok(hits)
}

fn handle_remember(
    memvid: &mut Memvid,
    content: &str,
    title: Option<&str>,
    tags: &[(String, String)],
) -> Result<()> {
    let mut builder = PutOptions::builder()
        // Explicitly set search_text so tantivy indexes the content directly,
        // bypassing the extraction pipeline which may silently produce empty text
        // for short entries (< 2400 chars).
        .search_text(content);
    if let Some(t) = title {
        builder = builder.title(t);
    }
    for (key, value) in tags {
        builder = builder.tag(key, value);
    }
    let opts = builder.build();

    memvid
        .put_bytes_with_options(content.as_bytes(), opts)
        .context("Failed to store memory")?;
    memvid.commit().context("Failed to commit memory")?;

    info!(
        title = title.unwrap_or("<untitled>"),
        content_len = content.len(),
        "Stored new memory"
    );

    Ok(())
}

fn handle_clear(memvid: &mut Memvid, path: &Path) -> Result<()> {
    warn!(path = %path.display(), "Clearing all agent memory");

    let mut fresh = Memvid::create(path).context("Failed to recreate memory file")?;
    fresh
        .enable_lex()
        .context("Failed to enable lexical search on cleared memory file")?;
    *memvid = fresh;

    Ok(())
}

fn handle_stats(memvid: &mut Memvid, path: &Path) -> Result<MemoryStats> {
    let file_size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let request = make_search_request("", 0, 0);
    let entry_count = memvid.search(request).map(|r| r.total_hits).unwrap_or(0);

    Ok(MemoryStats {
        entry_count,
        file_size_bytes,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_remember_and_search_within_session() {
        let dir = tempfile::tempdir().unwrap();
        let svc = MemoryService::open_or_create(dir.path()).await.unwrap();

        svc.remember(
            "The user's favorite color is blue",
            Some("Favorite color"),
            &[],
        )
        .await
        .unwrap();

        let hits = svc.search("favorite color", None).await.unwrap();
        assert!(!hits.is_empty(), "Should find the stored memory");
        assert!(
            hits[0].text.contains("blue"),
            "Hit text should contain 'blue', got: {}",
            hits[0].text
        );
    }

    /// BM25 lexical search requires word overlap — "fruits" won't find "bananas".
    /// This test documents that limitation: without the `vec` feature (vector
    /// similarity search), only keyword matches work.
    #[tokio::test]
    async fn test_lex_only_requires_keyword_overlap() {
        let dir = tempfile::tempdir().unwrap();
        let svc = MemoryService::open_or_create(dir.path()).await.unwrap();

        svc.remember("I like bananas", None, &[]).await.unwrap();

        // Exact keyword match → should find it
        let hits = svc.search("bananas", None).await.unwrap();
        assert!(!hits.is_empty(), "Exact keyword 'bananas' should match");

        // Partial keyword match → should find it (the word "like" appears)
        let hits = svc.search("like", None).await.unwrap();
        assert!(!hits.is_empty(), "Keyword 'like' should match");

        // Semantic query with no word overlap → BM25 cannot match
        let hits = svc.search("fruits", None).await.unwrap();
        assert!(
            hits.is_empty(),
            "BM25 cannot match 'fruits' to 'bananas' — needs vec feature for semantic search"
        );
    }

    #[tokio::test]
    async fn test_memory_persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();

        // Session 1: store a memory
        {
            let svc = MemoryService::open_or_create(dir.path()).await.unwrap();
            svc.remember(
                "The project uses PostgreSQL for the database",
                Some("Database choice"),
                &[("project", "chatty")],
            )
            .await
            .unwrap();
        }
        // MemoryService is dropped here, simulating app shutdown

        // Verify the .mv2 file exists on disk
        let mv2_path = dir.path().join("memory.mv2");
        assert!(
            mv2_path.exists(),
            "memory.mv2 should exist after remember+commit"
        );

        // Session 2: reopen and search
        {
            let svc = MemoryService::open_or_create(dir.path()).await.unwrap();
            let hits = svc.search("database", None).await.unwrap();
            assert!(
                !hits.is_empty(),
                "Should find memory from previous session, but got 0 results"
            );
            assert!(
                hits[0].text.contains("PostgreSQL"),
                "Hit should contain 'PostgreSQL', got: {}",
                hits[0].text
            );
        }
    }
}
