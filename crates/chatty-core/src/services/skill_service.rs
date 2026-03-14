use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::warn;

use super::embedding_service::EmbeddingService;
use super::memory_service::MemoryHit;
use crate::tools::save_skill_tool::SKILL_TITLE_PREFIX;

// ── Embedding cache ──────────────────────────────────────────────────────────

/// FNV-1a hash of `s`, returned as a 16-char hex string.
///
/// Used as a lightweight, dependency-free content fingerprint for cache
/// invalidation. Deterministic across process restarts (unlike `DefaultHasher`).
fn fnv1a_hash(s: &str) -> String {
    let mut hash: u64 = 14695981039346656037;
    for byte in s.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    format!("{:016x}", hash)
}

/// Load a cached skill embedding from `skill_dir` if the content fingerprint matches.
///
/// Cache layout (both files live inside the skill's own subdirectory):
/// - `SKILL.embedding`      — raw `f32` values, little-endian
/// - `SKILL.embedding.hash` — FNV-1a hex of the skill content that was embedded
///
/// Returns `None` when the cache is missing, unreadable, or stale.
async fn load_cached_embedding(skill_dir: &Path, content: &str) -> Option<Vec<f32>> {
    let expected = fnv1a_hash(content);
    let stored = tokio::fs::read_to_string(skill_dir.join("SKILL.embedding.hash"))
        .await
        .ok()?;
    if stored.trim() != expected {
        return None; // content changed → stale
    }
    let bytes = tokio::fs::read(skill_dir.join("SKILL.embedding"))
        .await
        .ok()?;
    if bytes.len() % 4 != 0 {
        return None; // corrupted
    }
    Some(
        bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect(),
    )
}

/// Write `embedding` alongside the skill file so future loads skip the API call.
async fn save_cached_embedding(skill_dir: &Path, content: &str, embedding: &[f32]) {
    let bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
    if let Err(e) = tokio::fs::write(skill_dir.join("SKILL.embedding"), &bytes).await {
        warn!(error = ?e, "Failed to write skill embedding cache");
        return;
    }
    let hash = fnv1a_hash(content);
    if let Err(e) = tokio::fs::write(skill_dir.join("SKILL.embedding.hash"), hash).await {
        warn!(error = ?e, "Failed to write skill embedding hash");
    }
}

// ── Scoring helpers ──────────────────────────────────────────────────────────

/// Cosine similarity between two vectors, clamped to [0, 1].
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        (dot / (norm_a * norm_b)).clamp(0.0, 1.0)
    }
}

/// Keyword overlap score: fraction of query words (len > 2) found in the skill text.
/// Returns 0.5 when there are no usable query words (neutral, not excluded).
fn keyword_overlap_score(query_words: &HashSet<String>, skill_name: &str, content: &str) -> f32 {
    if query_words.is_empty() {
        return 0.5;
    }
    let haystack = format!("{} {}", skill_name, content).to_lowercase();
    let matches = query_words
        .iter()
        .filter(|w| haystack.contains(w.as_str()))
        .count();
    matches as f32 / query_words.len() as f32
}

// ── SkillService ─────────────────────────────────────────────────────────────

/// Loads filesystem-based skills from one or two directories and scores them
/// against a query, with on-disk embedding caching to avoid redundant API calls.
///
/// ## Skill directories
/// - **Workspace**: `<workspace>/.claude/skills/` — project-local skills
/// - **Global**:    `<data_dir>/chatty/skills/`   — permanent user skills
///   - Linux:   `~/.local/share/chatty/skills/`
///   - macOS:   `~/Library/Application Support/chatty/skills/`
///   - Windows: `%APPDATA%\chatty\skills\`
///
/// Each directory is scanned for immediate subdirectories that contain a
/// `SKILL.md` (or `skill.md`) file. The subdirectory name becomes the skill
/// name. Embeddings are cached as sidecar files inside each skill subdirectory.
///
/// ## Scoring
/// When a `query_embedding` is provided skills are scored by cosine similarity
/// against their cached embeddings. If no cache exists the embedding is computed
/// via the service's `EmbeddingService` and written to disk for future calls.
/// Falls back to keyword overlap when no embedding is available.
#[derive(Clone)]
pub struct SkillService {
    global_skills_dir: PathBuf,
    embedding_service: Option<EmbeddingService>,
}

impl SkillService {
    /// Create a new `SkillService`.
    ///
    /// `embedding_service` is optional; pass `None` to use keyword-only scoring.
    pub fn new(embedding_service: Option<EmbeddingService>) -> Self {
        let global_skills_dir = dirs::data_dir()
            .map(|d| d.join("chatty").join("skills"))
            .unwrap_or_else(|| PathBuf::from(".chatty_skills"));
        Self {
            global_skills_dir,
            embedding_service,
        }
    }

    /// Load skill hits from both the workspace and global directories.
    ///
    /// Skills are scored by cosine similarity (cached) or keyword overlap.
    /// The caller should sort and truncate the returned hits together with
    /// any persisted memory hits before injecting them into context.
    pub async fn load_hits(
        &self,
        query: &str,
        query_embedding: Option<&[f32]>,
        workspace_skills_dir: Option<&Path>,
    ) -> Vec<MemoryHit> {
        let mut hits = Vec::new();
        if let Some(dir) = workspace_skills_dir {
            hits.extend(self.load_from_dir(dir, query, query_embedding).await);
        }
        hits.extend(
            self.load_from_dir(&self.global_skills_dir, query, query_embedding)
                .await,
        );
        hits
    }

    /// Scan a single skills directory and return scored `MemoryHit` objects.
    async fn load_from_dir(
        &self,
        skills_dir: &Path,
        query: &str,
        query_embedding: Option<&[f32]>,
    ) -> Vec<MemoryHit> {
        let query_words: HashSet<String> = query
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 2)
            .map(|w| w.to_lowercase())
            .collect();

        let mut hits = Vec::new();

        let mut dir = match tokio::fs::read_dir(skills_dir).await {
            Ok(d) => d,
            Err(_) => return hits,
        };

        while let Ok(Some(entry)) = dir.next_entry().await {
            let path = entry.path();

            let is_dir = tokio::fs::metadata(&path)
                .await
                .map(|m| m.is_dir())
                .unwrap_or(false);
            if !is_dir {
                continue;
            }

            // Try SKILL.md then skill.md
            let mut content: Option<String> = None;
            for name in &["SKILL.md", "skill.md"] {
                if let Ok(c) = tokio::fs::read_to_string(path.join(name)).await
                    && !c.trim().is_empty()
                {
                    content = Some(c);
                    break;
                }
            }
            let content = match content {
                Some(c) => c,
                None => continue,
            };

            let skill_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");

            let score = if let Some(query_emb) = query_embedding {
                // Resolve skill embedding: cached → compute+cache → keyword fallback
                let skill_emb = match load_cached_embedding(&path, &content).await {
                    Some(emb) => Some(emb),
                    None => match &self.embedding_service {
                        Some(svc) => match svc.embed(&content).await {
                            Ok(emb) => {
                                save_cached_embedding(&path, &content, &emb).await;
                                Some(emb)
                            }
                            Err(e) => {
                                warn!(
                                    error = ?e,
                                    skill = %skill_name,
                                    "Failed to embed local skill, using keyword score"
                                );
                                None
                            }
                        },
                        None => None,
                    },
                };
                skill_emb
                    .as_deref()
                    .map(|emb| cosine_similarity(query_emb, emb))
                    .unwrap_or_else(|| keyword_overlap_score(&query_words, skill_name, &content))
            } else {
                keyword_overlap_score(&query_words, skill_name, &content)
            };

            hits.push(MemoryHit {
                text: content,
                title: Some(format!("{}{}", SKILL_TITLE_PREFIX, skill_name)),
                score,
            });
        }

        hits
    }
}
