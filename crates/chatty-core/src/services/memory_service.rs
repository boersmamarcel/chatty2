use anyhow::{Context, Result};
#[cfg(feature = "memory")]
use memvid_core::{DocMetadata, Memvid, MemvidError, PutOptions, SearchRequest};
use std::path::{Path, PathBuf};
use tokio::sync::{mpsc, oneshot};
#[cfg(feature = "memory")]
use tracing::{error, info, warn};

/// Maximum number of search results returned by default
#[cfg_attr(not(feature = "memory"), allow(dead_code))]
const DEFAULT_TOP_K: usize = 5;

/// Snippet length for search results
#[cfg_attr(not(feature = "memory"), allow(dead_code))]
const SNIPPET_CHARS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryHitSource {
    Memory,
    WorkspaceSkillFile,
    GlobalSkillFile,
}

/// A single memory search result
#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryHit {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "relevance_score")]
    pub score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<MemoryHitSource>,
    /// Frame ID in the memvid store (used for deletion). Not serialized to LLM output.
    #[serde(skip)]
    pub frame_id: Option<u64>,
}

/// Statistics about the memory store
#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryStats {
    pub entry_count: usize,
    pub file_size_bytes: u64,
}

/// Commands sent to the dedicated memory thread.
#[cfg_attr(not(feature = "memory"), allow(dead_code))]
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
    RememberWithEmbedding {
        content: String,
        embedding: Vec<f32>,
        title: Option<String>,
        tags: Vec<(String, String)>,
        reply: oneshot::Sender<Result<()>>,
    },
    EnableVec {
        reply: oneshot::Sender<Result<()>>,
    },
    SetVecModel {
        model: String,
        reply: oneshot::Sender<Result<()>>,
    },
    SearchVec {
        query_embedding: Vec<f32>,
        limit: usize,
        reply: oneshot::Sender<Result<Vec<MemoryHit>>>,
    },
    List {
        query: String,
        limit: usize,
        reply: oneshot::Sender<Result<Vec<MemoryHit>>>,
    },
    Clear {
        reply: oneshot::Sender<Result<()>>,
    },
    Stats {
        reply: oneshot::Sender<Result<MemoryStats>>,
    },
    Delete {
        frame_id: u64,
        reply: oneshot::Sender<Result<()>>,
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

    /// Delete a single memory entry by frame ID.
    pub async fn delete(&self, frame_id: u64) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(MemoryCommand::Delete {
                frame_id,
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("Memory thread stopped"))?;

        reply_rx
            .await
            .context("Memory thread dropped reply channel")?
    }

    /// List stored memories, optionally filtered by a search query.
    ///
    /// If `query` is empty, returns up to `limit` recent entries from the store.
    /// If `query` is non-empty, performs a full-text search and returns ranked results.
    pub async fn list_memories(&self, query: &str, limit: usize) -> Result<Vec<MemoryHit>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(MemoryCommand::List {
                query: query.to_string(),
                limit,
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("Memory thread stopped"))?;

        reply_rx
            .await
            .context("Memory thread dropped reply channel")?
    }

    /// Store a memory entry with a pre-computed embedding vector.
    ///
    /// The embedding is stored alongside the text in the HNSW vector index,
    /// enabling semantic similarity search via `search_vec()`.
    pub async fn remember_with_embedding(
        &self,
        content: &str,
        embedding: Vec<f32>,
        title: Option<&str>,
        tags: &[(&str, &str)],
    ) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(MemoryCommand::RememberWithEmbedding {
                content: content.to_string(),
                embedding,
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

    /// Enable the HNSW vector index on the memvid file.
    pub async fn enable_vec(&self) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(MemoryCommand::EnableVec { reply: reply_tx })
            .await
            .map_err(|_| anyhow::anyhow!("Memory thread stopped"))?;

        reply_rx
            .await
            .context("Memory thread dropped reply channel")?
    }

    /// Bind the vector index to a specific embedding model identifier.
    ///
    /// Returns an error if the index is already bound to a different model
    /// (model mismatch — the user must purge memory to switch models).
    pub async fn set_vec_model(&self, model: &str) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(MemoryCommand::SetVecModel {
                model: model.to_string(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("Memory thread stopped"))?;

        reply_rx
            .await
            .context("Memory thread dropped reply channel")?
    }

    /// Search memory using a pre-computed query embedding vector (semantic search).
    ///
    /// Returns results ranked by vector similarity (L2 distance, lower = better).
    /// The distance is converted to a score for consistency with BM25 results.
    pub async fn search_vec(
        &self,
        query_embedding: Vec<f32>,
        top_k: Option<usize>,
    ) -> Result<Vec<MemoryHit>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(MemoryCommand::SearchVec {
                query_embedding,
                limit: top_k.unwrap_or(DEFAULT_TOP_K),
                reply: reply_tx,
            })
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
#[cfg(feature = "memory")]
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
                let (store, result) = write_with_recovery(memvid, &path, &content, |m| {
                    handle_remember(m, &content, title.as_deref(), &tags)
                });
                let _ = reply.send(result);
                match store {
                    Some(m) => memvid = m,
                    None => break,
                }
            }
            MemoryCommand::RememberWithEmbedding {
                content,
                embedding,
                title,
                tags,
                reply,
            } => {
                let (store, result) = write_with_recovery(memvid, &path, &content, |m| {
                    handle_remember_with_embedding(
                        m,
                        &content,
                        embedding.clone(),
                        title.as_deref(),
                        &tags,
                    )
                });
                let _ = reply.send(result);
                match store {
                    Some(m) => memvid = m,
                    None => break,
                }
            }
            MemoryCommand::EnableVec { reply } => {
                let result = memvid
                    .enable_vec()
                    .context("Failed to enable vector search");
                let _ = reply.send(result);
            }
            MemoryCommand::SetVecModel { model, reply } => {
                let result = memvid
                    .set_vec_model(&model)
                    .context("Failed to set vector model");
                let _ = reply.send(result);
            }
            MemoryCommand::SearchVec {
                query_embedding,
                limit,
                reply,
            } => {
                let result = handle_search_vec(&mut memvid, &query_embedding, limit);
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
            MemoryCommand::List {
                query,
                limit,
                reply,
            } => {
                let result = if query.is_empty() {
                    handle_list_all(&mut memvid, limit)
                } else {
                    handle_search(&mut memvid, &query, limit)
                };
                let _ = reply.send(result);
            }
            MemoryCommand::Delete { frame_id, reply } => {
                let result = handle_delete(&mut memvid, frame_id);
                let _ = reply.send(result);
            }
        }
    }

    info!("Memory thread shutting down");
}

/// How often an operation that lost the fork/flock race (see
/// `write_with_recovery`) is retried before giving up (AGE-1637). Each retry
/// can lose the same race; under a test's back-to-back spawning about one in
/// four does, so this leaves a margin far beyond what a session's own shell
/// and MCP spawns produce.
#[cfg(feature = "memory")]
const WRITER_RACE_RETRIES: u32 = 30;

/// Backoff before retry `n` is `n` times this, capped at
/// `WRITER_RACE_MAX_BACKOFF`.
#[cfg(feature = "memory")]
const WRITER_RACE_BACKOFF: std::time::Duration = std::time::Duration::from_millis(10);

#[cfg(feature = "memory")]
const WRITER_RACE_MAX_BACKOFF: std::time::Duration = std::time::Duration::from_millis(100);

/// Run a memory write (`put` + `commit`) and recover the store when tantivy
/// could not re-take its writer lock (AGE-1637).
///
/// memvid's `TantivyEngine::commit` drops its `IndexWriter`, which releases the
/// `flock` on `.tantivy-writer.lock`, and then creates a new writer at once. If
/// any thread of this process forks in between (shell tool, git, MCP stdio
/// servers), the child holds a copy of the lock descriptor until it execs, so
/// the new writer fails with `LockBusy`. When that happens on the engine memvid
/// keeps, the engine is left without a writer and every later write fails with
/// "tantivy index writer unavailable" until the store is reopened.
///
/// Recovery is the same as an app restart: drop the handle (it holds the
/// `.mv2` lock) and open the file again, which builds a fresh tantivy engine in
/// a new directory (memvid has no public call that replaces the engine of an
/// open handle). A failed write has already appended its record to the file's
/// WAL (memvid's `put` appends before it indexes, and a failed `commit` rolls
/// the file back to that state), and `Memvid::open` replays pending WAL
/// records. So after reopening, the write is only run again when its content is
/// not among the frames the reopen added; running it unconditionally would
/// store the memory twice.
///
/// Returns the handle to keep using, or `None` when the store could not be
/// reopened; the memory thread then stops and later calls fail with "Memory
/// thread stopped".
#[cfg(feature = "memory")]
fn write_with_recovery(
    mut memvid: Memvid,
    path: &Path,
    content: &str,
    mut write: impl FnMut(&mut Memvid) -> Result<()>,
) -> (Option<Memvid>, Result<()>) {
    let frames_before = memvid.frame_count();
    match write(&mut memvid) {
        Err(e) if is_writer_lock_race(&e) => warn!(
            path = %path.display(),
            error = %format!("{e:#}"),
            "Memory index writer lock lost to a forked process; reopening the memory file"
        ),
        result => return (Some(memvid), result),
    }
    drop(memvid);

    let recovered = retry_on_writer_lock_race(path, || {
        let mut reopened = open_memvid(path)?;
        if contains_new_frame_with_payload(&mut reopened, frames_before, content) {
            info!("Memory write recovered from the WAL after reopening");
            return Ok((reopened, Ok(())));
        }
        match write(&mut reopened) {
            Err(e) if is_writer_lock_race(&e) => Err(e),
            result => Ok((reopened, result)),
        }
    });
    match recovered {
        Ok((reopened, result)) => (Some(reopened), result),
        Err(e) => {
            error!(
                path = %path.display(),
                error = %format!("{e:#}"),
                "Could not reopen the memory file; agent memory is unavailable until restart"
            );
            (None, Err(e.context("Memory store could not be reopened")))
        }
    }
}

/// Run `op` again, with a short backoff, while it fails on the tantivy
/// writer-lock race, at most `WRITER_RACE_RETRIES` times.
#[cfg(feature = "memory")]
fn retry_on_writer_lock_race<T>(path: &Path, mut op: impl FnMut() -> Result<T>) -> Result<T> {
    let mut retry = 0;
    loop {
        match op() {
            Err(e) if is_writer_lock_race(&e) && retry < WRITER_RACE_RETRIES => {
                retry += 1;
                warn!(
                    path = %path.display(),
                    retry,
                    error = %format!("{e:#}"),
                    "Memory index writer lock held by a forked process; retrying"
                );
                std::thread::sleep((WRITER_RACE_BACKOFF * retry).min(WRITER_RACE_MAX_BACKOFF));
            }
            result => return result,
        }
    }
}

/// Whether `err` is tantivy failing to (re)take its writer lock, or the
/// writer-less engine that failure leaves behind.
#[cfg(feature = "memory")]
fn is_writer_lock_race(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<MemvidError>(),
            Some(MemvidError::Tantivy { reason })
                if reason.contains("LockBusy") || reason.contains("index writer unavailable")
        )
    })
}

/// Open an existing memory file and enable lexical search, restoring the file
/// from a copy when the open fails.
///
/// `Memvid::open` replays pending WAL records into the file in place, outside
/// memvid's staged commit. When the tantivy lock race hits that replay, the
/// open fails after the table of contents was overwritten, and the file can no
/// longer be opened at all (seen in testing under a fork storm, AGE-1637). So
/// the file is copied before each attempt and put back when the attempt fails.
#[cfg(feature = "memory")]
fn open_memvid(path: &Path) -> Result<Memvid> {
    let backup = path.with_extension("mv2.open-backup");
    std::fs::copy(path, &backup).context("Failed to copy memory file before opening it")?;

    let opened = Memvid::open(path)
        .context("Failed to open memory file")
        .and_then(|mut memvid| {
            memvid
                .enable_lex()
                .context("Failed to enable lexical search on memory file")?;
            Ok(memvid)
        });
    if opened.is_err() {
        // The failed handle is dropped by now, so the file is free to restore.
        // On a failed restore the copy stays next to the file.
        std::fs::rename(&backup, path).with_context(|| {
            format!(
                "Failed to restore memory file from {} after a failed open",
                backup.display()
            )
        })?;
    } else if let Err(e) = std::fs::remove_file(&backup) {
        warn!(path = %backup.display(), error = ?e, "Failed to remove memory file copy");
    }
    opened
}

/// Whether a frame at index `from` or later holds `content` as its payload.
#[cfg(feature = "memory")]
fn contains_new_frame_with_payload(memvid: &mut Memvid, from: usize, content: &str) -> bool {
    (from..memvid.frame_count()).any(|id| {
        memvid
            .frame_canonical_payload(id as u64)
            .is_ok_and(|payload| payload == content.as_bytes())
    })
}

/// Open or create the Memvid file and enable lexical search.
///
/// Opening and creating both commit, so both are retried on the tantivy
/// writer-lock race (AGE-1637); a retried create truncates the half-made file.
///
/// After opening an existing file, runs a health check to detect a corrupted
/// lex index. If the index is broken (e.g. empty manifest from a previous bug),
/// recreates the file to restore working lex search. This is a one-time
/// self-healing step — data stored with a broken lex index is unrecoverable.
#[cfg(feature = "memory")]
fn init_memvid(path: &Path) -> Result<Memvid> {
    let existed = path.exists();
    if existed {
        info!(path = %path.display(), "Opening existing memory file");
    } else {
        info!(path = %path.display(), "Creating new memory file");
    }
    let mut memvid = retry_on_writer_lock_race(path, || {
        if existed {
            return open_memvid(path);
        }
        let mut memvid = Memvid::create(path).context("Failed to create memory file")?;
        memvid
            .enable_lex()
            .context("Failed to enable lexical search on memory file")?;
        Ok(memvid)
    })?;

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
            temporal: None,
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

#[cfg(feature = "memory")]
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
            source: Some(MemoryHitSource::Memory),
            frame_id: Some(hit.frame_id),
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

/// List all memory entries without a search query by iterating frames directly.
///
/// Returns up to `limit` entries in reverse order (newest first).
/// Uses `frame_by_id` + `frame_text_by_id` to read title and content,
/// avoiding the Tantivy search path which rejects empty queries.
#[cfg(feature = "memory")]
fn handle_list_all(memvid: &mut Memvid, limit: usize) -> Result<Vec<MemoryHit>> {
    let count = memvid.frame_count();
    if count == 0 {
        return Ok(Vec::new());
    }

    let mut hits = Vec::with_capacity(limit.min(count));

    // Iterate newest-first (highest frame ID first)
    for frame_id in (0..count as u64).rev().take(limit) {
        let frame = match memvid.frame_by_id(frame_id) {
            Ok(f) => f,
            Err(_) => continue,
        };

        // Prefer search_text (set explicitly by handle_remember), fall back to
        // frame_text_by_id, which reads the stored text back from the frame
        // (it does not run memvid's extraction pipeline).
        let text = if let Some(ref st) = frame.search_text {
            st.clone()
        } else {
            match memvid.frame_text_by_id(frame_id) {
                Ok(t) => t,
                Err(_) => continue,
            }
        };

        hits.push(MemoryHit {
            text,
            title: frame.title.clone(),
            score: 0.0,
            source: Some(MemoryHitSource::Memory),
            frame_id: Some(frame_id),
        });
    }

    info!(
        total_frames = count,
        returned = hits.len(),
        "Listed all memories"
    );

    Ok(hits)
}

/// Put options for a memory: the content is plain text and is stored as text,
/// never run through memvid's extraction pipeline.
///
/// memvid's `put_internal` runs its extractor when
/// `need_search_text || need_metadata || auto_tag`, and the extractor routes
/// the bytes by content sniffing: text that starts with `%PDF` goes to lopdf,
/// whose nested-array recursion (RUSTSEC-2026-0187) can overflow the stack and
/// abort the process (AGE-1080). Each of the three inputs is turned off here:
/// - `search_text` is the content itself (tantivy indexes it directly; the
///   extractor could also produce empty text for short entries), so
///   `need_search_text` is false for any content that is not blank;
/// - `metadata` names the content `text/plain`, so `need_metadata` is false;
/// - `auto_tag(false)`: memvid's derived keyword tags, labels and dates are
///   not added (the caller's own `tags` still are).
#[cfg(feature = "memory")]
fn remember_put_options(
    content: &str,
    title: Option<&str>,
    tags: &[(String, String)],
) -> PutOptions {
    let mut builder = PutOptions::builder()
        .search_text(content)
        .metadata(DocMetadata {
            mime: Some("text/plain".to_string()),
            ..DocMetadata::default()
        })
        .auto_tag(false);
    if let Some(t) = title {
        builder = builder.title(t);
    }
    for (key, value) in tags {
        builder = builder.tag(key, value);
    }
    builder.build()
}

#[cfg(feature = "memory")]
fn handle_remember(
    memvid: &mut Memvid,
    content: &str,
    title: Option<&str>,
    tags: &[(String, String)],
) -> Result<()> {
    let opts = remember_put_options(content, title, tags);

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

#[cfg(feature = "memory")]
fn handle_remember_with_embedding(
    memvid: &mut Memvid,
    content: &str,
    embedding: Vec<f32>,
    title: Option<&str>,
    tags: &[(String, String)],
) -> Result<()> {
    let opts = remember_put_options(content, title, tags);

    memvid
        .put_with_embedding_and_options(content.as_bytes(), embedding, opts)
        .context("Failed to store memory with embedding")?;
    memvid.commit().context("Failed to commit memory")?;

    info!(
        title = title.unwrap_or("<untitled>"),
        content_len = content.len(),
        "Stored new memory with embedding"
    );

    Ok(())
}

#[cfg(feature = "memory")]
fn handle_search_vec(
    memvid: &mut Memvid,
    query_embedding: &[f32],
    limit: usize,
) -> Result<Vec<MemoryHit>> {
    let vec_hits = match memvid.search_vec(query_embedding, limit) {
        Ok(hits) => hits,
        Err(e) => {
            warn!(error = ?e, "Vector search returned error, treating as empty");
            return Ok(Vec::new());
        }
    };

    let mut results = Vec::with_capacity(vec_hits.len());
    for hit in vec_hits {
        // Get text content and title from the frame
        let text = match memvid.frame_text_by_id(hit.frame_id) {
            Ok(text) => text,
            Err(e) => {
                warn!(
                    frame_id = hit.frame_id,
                    error = ?e,
                    "Failed to read frame text for vec hit, skipping"
                );
                continue;
            }
        };

        let title = memvid
            .frame_by_id(hit.frame_id)
            .ok()
            .and_then(|f| f.title.clone());

        // Convert L2 distance to a similarity score (1 / (1 + distance)).
        // Lower distance = higher score, range (0, 1].
        let score = 1.0 / (1.0 + hit.distance);

        results.push(MemoryHit {
            text,
            title,
            score,
            source: Some(MemoryHitSource::Memory),
            frame_id: Some(hit.frame_id),
        });
    }

    info!(hit_count = results.len(), "Vector search completed");

    Ok(results)
}

#[cfg(feature = "memory")]
fn handle_clear(memvid: &mut Memvid, path: &Path) -> Result<()> {
    warn!(path = %path.display(), "Clearing all agent memory");

    let mut fresh = Memvid::create(path).context("Failed to recreate memory file")?;
    fresh
        .enable_lex()
        .context("Failed to enable lexical search on cleared memory file")?;
    *memvid = fresh;

    Ok(())
}

#[cfg(feature = "memory")]
fn handle_delete(memvid: &mut Memvid, frame_id: u64) -> Result<()> {
    memvid
        .delete_frame(frame_id)
        .context("Failed to delete memory frame")?;
    memvid.commit().context("Failed to commit after delete")?;

    info!(frame_id = frame_id, "Deleted memory entry");
    Ok(())
}

#[cfg(feature = "memory")]
fn handle_stats(memvid: &mut Memvid, path: &Path) -> Result<MemoryStats> {
    let file_size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let entry_count = memvid.frame_count();

    Ok(MemoryStats {
        entry_count,
        file_size_bytes,
    })
}

/// Build a SearchRequest with sensible defaults for optional fields.
#[cfg(feature = "memory")]
fn make_search_request(query: &str, top_k: usize, snippet_chars: usize) -> SearchRequest {
    SearchRequest {
        query: query.to_string(),
        top_k,
        snippet_chars,
        uri: None,
        scope: None,
        cursor: None,
        temporal: None,
        as_of_frame: None,
        as_of_ts: None,
        no_sketch: false,
        acl_context: None,
        acl_enforcement_mode: Default::default(),
    }
}

/// Without the `memory` feature there is no store: opening fails, so callers
/// run as they do when the memory file cannot be opened.
#[cfg(not(feature = "memory"))]
fn run_memory_thread(
    _path: PathBuf,
    _cmd_rx: mpsc::Receiver<MemoryCommand>,
    init_tx: oneshot::Sender<Result<()>>,
) {
    let _ = init_tx.send(Err(anyhow::anyhow!(
        "agent memory is not compiled in (feature `memory`)"
    )));
}

/// Returns the platform-specific data directory for memory storage.
pub fn memory_data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("chatty"))
}

#[cfg(all(test, feature = "memory"))]
mod tests {
    use super::*;

    /// Serialize memory-service tests to avoid Tantivy file-lock contention
    /// when multiple `MemoryService` instances run in parallel test threads.
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn test_remember_and_search_within_session() {
        let _guard = SERIAL.lock().await;
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
        let _guard = SERIAL.lock().await;
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

    /// A structurally valid PDF whose Catalog holds an array nested `depth`
    /// levels deep. lopdf 0.39 parses nested arrays with unbounded recursion
    /// (RUSTSEC-2026-0187), so handing this to `lopdf::Document::load_mem`
    /// overflows the stack and aborts the process. The xref offsets are
    /// computed so lopdf gets past its early structural checks and recurses.
    fn crafted_nested_array_pdf(depth: usize) -> String {
        let nested = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        let mut pdf = String::from("%PDF-1.4\n");
        let catalog_offset = pdf.len();
        pdf.push_str(&format!(
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R /X {nested} >>\nendobj\n"
        ));
        let pages_offset = pdf.len();
        pdf.push_str("2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
        let xref_offset = pdf.len();
        pdf.push_str(&format!(
            "xref\n0 3\n0000000000 65535 f \n{catalog_offset:010} 00000 n \n\
             {pages_offset:010} 00000 n \ntrailer\n<< /Size 3 /Root 1 0 R >>\n\
             startxref\n{xref_offset}\n%%EOF\n"
        ));
        pdf
    }

    /// AGE-1080: `remember` takes model-supplied text. Text that happens to be
    /// a PDF must be stored as text, never handed to memvid's extractor (which
    /// routes `%PDF` bytes to lopdf). If the extractor is reachable again this
    /// test does not fail politely: lopdf overflows the stack and the test
    /// process aborts with SIGABRT.
    #[tokio::test]
    async fn test_remember_crafted_pdf_does_not_reach_pdf_parser() {
        let _guard = SERIAL.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let svc = MemoryService::open_or_create(dir.path()).await.unwrap();

        let pdf = crafted_nested_array_pdf(1_000);
        assert!(pdf.starts_with("%PDF"));

        svc.remember(&pdf, Some("crafted pdf"), &[]).await.unwrap();

        // Stored verbatim as text and still findable by a word it contains.
        let hits = svc.search("Catalog", None).await.unwrap();
        assert!(!hits.is_empty(), "the PDF-shaped text should be stored");
        assert!(hits[0].text.starts_with("%PDF"), "got: {}", hits[0].text);
    }

    /// Both remember paths (`handle_remember` and
    /// `handle_remember_with_embedding`) build their options here. memvid runs
    /// its extractor when `need_search_text || need_metadata || auto_tag`, so
    /// all three must be off: `auto_tag(false)` alone is not enough.
    #[test]
    fn test_remember_put_options_keep_memvid_extractor_off() {
        let opts = remember_put_options("%PDF-1.4", Some("t"), &[]);
        assert!(!opts.auto_tag, "auto_tag would run the extractor");
        assert!(
            opts.metadata.is_some(),
            "missing metadata would run the extractor"
        );
        assert_eq!(opts.search_text.as_deref(), Some("%PDF-1.4"));
    }

    #[tokio::test]
    async fn test_memory_persists_across_reopen() {
        let _guard = SERIAL.lock().await;
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

    /// Spawns short-lived subprocesses on another thread until dropped, waiting
    /// `pause` between them, so forks keep landing inside memvid's commits
    /// (AGE-1637). Each child holds a copy of every open descriptor, tantivy's
    /// writer lock file included, until it execs.
    #[cfg(unix)]
    struct ForkStorm {
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    #[cfg(unix)]
    impl ForkStorm {
        fn start(pause: std::time::Duration) -> Self {
            use std::sync::atomic::Ordering;
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = stop.clone();
            let thread = std::thread::spawn(move || {
                while !flag.load(Ordering::Relaxed) {
                    std::process::Command::new("true")
                        .status()
                        .expect("spawn `true`");
                    std::thread::sleep(pause);
                }
            });
            Self {
                stop,
                thread: Some(thread),
            }
        }
    }

    #[cfg(unix)]
    impl Drop for ForkStorm {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                thread.join().expect("fork storm thread panicked");
            }
        }
    }

    /// Frames whose payload is exactly `content`.
    fn frames_with_payload(memvid: &mut Memvid, content: &str) -> usize {
        (0..memvid.frame_count() as u64)
            .filter(|&id| {
                memvid
                    .frame_canonical_payload(id)
                    .is_ok_and(|p| p == content.as_bytes())
            })
            .count()
    }

    /// Open tantivy writer-lock descriptors of this process and their files.
    #[cfg(target_os = "linux")]
    fn writer_lock_fds() -> Vec<(i32, PathBuf)> {
        let Ok(entries) = std::fs::read_dir("/proc/self/fd") else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let fd = entry.file_name().to_str()?.parse::<i32>().ok()?;
                let target = std::fs::read_link(entry.path()).ok()?;
                target
                    .ends_with(".tantivy-writer.lock")
                    .then_some((fd, target))
            })
            .collect()
    }

    /// Plays the forked child of AGE-1637 on another thread until dropped: once
    /// a tantivy writer lock file has been open for `delay`, it holds a
    /// duplicate of its descriptor until memvid has closed every other one. A
    /// duplicate shares the `flock`, so every writer memvid re-creates on that
    /// engine in the meantime fails with `LockBusy`.
    #[cfg(target_os = "linux")]
    struct LockSnatcher {
        delay: std::sync::Arc<std::sync::atomic::AtomicU64>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    #[cfg(target_os = "linux")]
    impl LockSnatcher {
        fn start() -> Self {
            use std::collections::HashMap;
            use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
            use std::sync::atomic::Ordering;
            use std::time::{Duration, Instant};

            let delay = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (delay_us, flag) = (delay.clone(), stop.clone());
            let thread = std::thread::spawn(move || {
                let mut first_seen: HashMap<PathBuf, Instant> = HashMap::new();
                let mut held: HashMap<PathBuf, OwnedFd> = HashMap::new();
                while !flag.load(Ordering::Relaxed) {
                    let ours: Vec<i32> = held.values().map(|fd| fd.as_raw_fd()).collect();
                    let open: Vec<(i32, PathBuf)> = writer_lock_fds()
                        .into_iter()
                        .filter(|(fd, _)| !ours.contains(fd))
                        .collect();
                    let wait = Duration::from_micros(delay_us.load(Ordering::Relaxed));
                    for (fd, lock) in &open {
                        let seen = *first_seen.entry(lock.clone()).or_insert_with(Instant::now);
                        if seen.elapsed() >= wait && !held.contains_key(lock) {
                            // SAFETY: only borrowed for the `dup`; a descriptor
                            // closed since the listing makes it fail or copy
                            // another file, which is closed again right away
                            // because no other descriptor names its path.
                            if let Ok(dup) =
                                unsafe { BorrowedFd::borrow_raw(*fd) }.try_clone_to_owned()
                            {
                                held.insert(lock.clone(), dup);
                            }
                        }
                    }
                    held.retain(|lock, _| open.iter().any(|(_, l)| l == lock));
                    std::thread::sleep(Duration::from_micros(100));
                }
            });
            Self {
                delay,
                stop,
                thread: Some(thread),
            }
        }

        fn set_delay(&self, delay: std::time::Duration) {
            self.delay.store(
                delay.as_micros() as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for LockSnatcher {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                thread.join().expect("lock snatcher thread panicked");
            }
        }
    }

    /// A handle whose tantivy engine lost its writer stays unable to write
    /// after the lock holder is gone; `write_with_recovery` reopens it, stores
    /// the memory exactly once, keeps the writes that failed while stuck, and
    /// leaves a handle that writes again (AGE-1637).
    ///
    /// memvid builds a new tantivy engine, with a new lock file, inside every
    /// commit, and the stuck state comes from losing the lock at the last of
    /// that commit's writer re-creations, so it cannot be staged from outside.
    /// The setup writes while `LockSnatcher` takes each new lock after a delay
    /// it bisects towards that last re-creation, until memvid reports the
    /// writer missing; from there on every step is deterministic.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_write_recovers_after_index_writer_is_lost() {
        let _guard = SERIAL.blocking_lock();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.mv2");
        let mut memvid = init_memvid(&path).unwrap();

        // Writes one memory; reports whether it was stored and whether it left
        // the handle stuck. Any other failure must be the lock race (forks of
        // tests running alongside cause it too).
        let write_and_check = |memvid: &mut Memvid| {
            let result = handle_remember(memvid, "written under contention", None, &[]);
            if let Err(e) = &result {
                assert!(is_writer_lock_race(e), "unexpected error: {e:#}");
            }
            let stuck = result
                .as_ref()
                .is_err_and(|e| format!("{e:#}").contains("index writer unavailable"));
            (result.is_ok(), stuck)
        };

        // How long one commit takes here, so the snatcher's delay can sweep it.
        let started = std::time::Instant::now();
        let (_, mut stuck) = write_and_check(&mut memvid);
        let mut commit = started.elapsed();

        // Bisect the delay: a commit that still fails took the lock too early
        // (before the engine's last writer re-creation), one that succeeds too
        // late.
        let snatcher = LockSnatcher::start();
        let (mut early, mut late) = (std::time::Duration::ZERO, commit * 2);
        // Bounded in time too, so a slow runner gives up (and skips) quickly.
        let staging_deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        for _ in 0..150 {
            if stuck || std::time::Instant::now() > staging_deadline {
                break;
            }
            let delay = (early + late) / 2;
            snatcher.set_delay(delay);
            let started = std::time::Instant::now();
            let (stored, now_stuck) = write_and_check(&mut memvid);
            stuck = now_stuck;
            if stored {
                late = delay;
                // Commits slow down as failed writes pile up in the WAL.
                commit = started.elapsed();
            } else {
                early = delay;
            }
            if late.saturating_sub(early) < commit / 50 {
                // Timing drifted out of the window; start over.
                (early, late) = (std::time::Duration::ZERO, commit * 2);
            }
        }
        drop(snatcher);
        if !stuck {
            // Staging depends on timing; test_remember_survives_concurrent_forks
            // still guards the property on every run.
            eprintln!("skipped: could not stage the stuck index-writer state on this machine");
            return;
        }

        // Nothing holds the lock any more, and the handle still cannot write.
        let err = handle_remember(&mut memvid, "written while stuck", None, &[]).unwrap_err();
        assert!(
            format!("{err:#}").contains("index writer unavailable"),
            "got: {err:#}"
        );

        let content = "The deploy key lives in the vault";
        let (store, result) = write_with_recovery(memvid, &path, content, |m| {
            handle_remember(m, content, None, &[])
        });
        result.unwrap();
        let mut memvid = store.expect("store reopened");
        assert_eq!(frames_with_payload(&mut memvid, content), 1);
        // The write that failed while stuck was in the WAL and is kept.
        assert_eq!(frames_with_payload(&mut memvid, "written while stuck"), 1);

        let after = "written after recovery";
        let (store, result) = write_with_recovery(memvid, &path, after, |m| {
            handle_remember(m, after, None, &[])
        });
        result.unwrap();
        let mut memvid = store.expect("store still open");
        assert_eq!(frames_with_payload(&mut memvid, after), 1);
        let hits = handle_search(&mut memvid, "vault", 5).unwrap();
        assert!(
            hits.iter().any(|h| h.text.contains(content)),
            "recovered memory is searchable: {hits:?}"
        );
    }

    /// Every `remember` succeeds, exactly once, while another thread keeps
    /// forking subprocesses (AGE-1637).
    #[cfg(unix)]
    #[tokio::test]
    async fn test_remember_survives_concurrent_forks() {
        const WRITES: usize = 6;
        let _guard = SERIAL.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let svc = MemoryService::open_or_create(dir.path()).await.unwrap();

        let storm = ForkStorm::start(std::time::Duration::from_millis(5));
        for i in 0..WRITES {
            svc.remember(&format!("stress fact marker{i}"), None, &[])
                .await
                .unwrap_or_else(|e| panic!("write {i} failed: {e:#}"));
        }
        drop(storm);

        assert_eq!(svc.stats().await.unwrap().entry_count, WRITES);
    }
}
