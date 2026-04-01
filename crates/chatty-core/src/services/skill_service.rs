use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::warn;

use super::embedding_service::EmbeddingService;
use super::memory_service::{MemoryHit, MemoryHitSource};
use crate::tools::save_skill_tool::SKILL_TITLE_PREFIX;

// ── Frontmatter helpers ──────────────────────────────────────────────────────

/// File names tried (in order) when searching for a skill's definition inside
/// a skill subdirectory.
const SKILL_FILE_NAMES: &[&str] = &["SKILL.md", "skill.md"];

/// Extract the `description` field from a SKILL.md YAML frontmatter block.
///
/// The frontmatter is a `---`-delimited YAML block at the top of the file.
/// Returns `None` if the file has no frontmatter or no `description` key.
pub fn extract_frontmatter_description(content: &str) -> Option<String> {
    let content = content.trim_start();
    if !content.starts_with("---") {
        return None;
    }
    let rest = &content[3..];
    // Frontmatter ends at the next `---` that starts on its own line.
    // Accept `\n---\n`, `\n---\r\n`, and `\n---` at end-of-string.
    let end = rest.find("\n---").filter(|&pos| {
        let after = &rest[pos + 4..]; // skip "\n---"
        after.is_empty() || after.starts_with('\n') || after.starts_with('\r')
    })?;
    let frontmatter = &rest[..end];
    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("description:") {
            let value = value.trim();
            // Strip optional surrounding quotes
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value);
            return Some(value.to_string());
        }
    }
    None
}

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

/// Synchronously list all available skills from the given directory.
///
/// Returns a list of `(name, description)` pairs for every skill subdirectory
/// that contains a `SKILL.md` or `skill.md` file.  Skills without a frontmatter
/// `description` field fall back to `"Skill: <name>"`.
///
/// This is a lightweight, blocking helper intended for UI use (e.g. populating
/// the slash-command picker) where async I/O would be inconvenient.
pub fn list_skills_from_dir(dir: &Path) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut skills = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(skill_name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        // Skip hidden directories and embedding cache files
        if skill_name.starts_with('.') {
            continue;
        }
        // Read SKILL.md or skill.md
        let content = SKILL_FILE_NAMES.iter().find_map(|name| {
            let s = std::fs::read_to_string(path.join(name)).ok()?;
            if s.trim().is_empty() { None } else { Some(s) }
        });
        let Some(content) = content else {
            continue;
        };
        let description = extract_frontmatter_description(&content)
            .unwrap_or_else(|| format!("Skill: {}", skill_name));
        skills.push((skill_name, description));
    }
    // Sort alphabetically for a stable display order
    skills.sort_by(|a, b| a.0.cmp(&b.0));
    skills
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

    /// Create a `SkillService` with a custom global skills directory.
    #[cfg(test)]
    pub fn with_global_dir(global_skills_dir: PathBuf) -> Self {
        Self {
            global_skills_dir,
            embedding_service: None,
        }
    }

    /// Return the path to the global skills directory.
    pub fn global_skills_dir(&self) -> &Path {
        &self.global_skills_dir
    }

    /// Synchronously list all skills from the workspace and global directories.
    ///
    /// Workspace skills are listed first, followed by global skills.  Duplicate
    /// names are deduplicated (workspace takes precedence).
    pub fn list_all_skills_sync(
        &self,
        workspace_skills_dir: Option<&Path>,
    ) -> Vec<(String, String)> {
        let mut seen = std::collections::HashSet::new();
        let mut result = Vec::new();

        let workspace_skills = workspace_skills_dir
            .map(list_skills_from_dir)
            .unwrap_or_default();
        for (name, desc) in workspace_skills {
            if seen.insert(name.clone()) {
                result.push((name, desc));
            }
        }

        let global_skills = list_skills_from_dir(&self.global_skills_dir);
        for (name, desc) in global_skills {
            if seen.insert(name.clone()) {
                result.push((name, desc));
            }
        }

        result
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
            for name in SKILL_FILE_NAMES {
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

            let source = if skills_dir == self.global_skills_dir.as_path() {
                MemoryHitSource::GlobalSkillFile
            } else {
                MemoryHitSource::WorkspaceSkillFile
            };
            // Store only the description so the context block stays slim.
            // The full content is still used above for scoring (embedding + keyword).
            // Filesystem skills can be expanded later with `read_skill`.
            let summary = extract_frontmatter_description(&content).unwrap_or_else(|| {
                // Fall back to the first non-empty, non-heading content line when
                // there is no frontmatter description.
                content
                    .lines()
                    .find(|l| {
                        let l = l.trim();
                        !l.is_empty() && !l.starts_with('#')
                    })
                    .unwrap_or("")
                    .to_string()
            });

            hits.push(MemoryHit {
                text: summary,
                title: Some(format!("{}{}", SKILL_TITLE_PREFIX, skill_name)),
                score,
                source: Some(source),
            });
        }

        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_description_from_standard_frontmatter() {
        let content = "---\nname: build-and-check\ndescription: Runs the full build pipeline.\nallowed-tools: Bash\n---\n\n# Body";
        assert_eq!(
            extract_frontmatter_description(content),
            Some("Runs the full build pipeline.".to_string())
        );
    }

    #[test]
    fn extract_description_with_quoted_value() {
        let content = "---\nname: my-skill\ndescription: \"A quoted description.\"\n---\n";
        assert_eq!(
            extract_frontmatter_description(content),
            Some("A quoted description.".to_string())
        );
    }

    #[test]
    fn returns_none_when_no_frontmatter() {
        let content = "# Just a markdown file\nNo frontmatter here.";
        assert!(extract_frontmatter_description(content).is_none());
    }

    #[test]
    fn returns_none_when_no_description_key() {
        let content = "---\nname: my-skill\nallowed-tools: Bash\n---\n# Body";
        assert!(extract_frontmatter_description(content).is_none());
    }

    #[tokio::test]
    async fn load_hits_returns_description_not_full_content() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("my-skill");
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();
        let content = "---\nname: my-skill\ndescription: Short description.\n---\n\n# Heading\n\nLong content that should NOT be in context.";
        tokio::fs::write(skill_dir.join("SKILL.md"), content)
            .await
            .unwrap();

        // Use an empty global dir so only the workspace skill is found.
        let empty_global = tempfile::tempdir().unwrap();
        let service = SkillService::with_global_dir(empty_global.path().to_path_buf());
        let hits = service
            .load_hits("my skill query", None, Some(tmp.path()))
            .await;

        assert_eq!(hits.len(), 1);
        // Only the description should be in `text`, not the full content
        assert_eq!(hits[0].text, "Short description.");
        assert!(!hits[0].text.contains("Long content"));
    }

    #[test]
    fn list_skills_from_dir_returns_name_and_description() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("fix-ci");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: fix-ci\ndescription: Diagnoses CI failures.\n---\n# Body",
        )
        .unwrap();

        let skills = list_skills_from_dir(tmp.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].0, "fix-ci");
        assert_eq!(skills[0].1, "Diagnoses CI failures.");
    }

    #[test]
    fn list_skills_from_dir_falls_back_to_skill_name_when_no_description() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "# Just a heading\nNo frontmatter.",
        )
        .unwrap();

        let skills = list_skills_from_dir(tmp.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].0, "my-skill");
        assert!(
            skills[0].1.contains("my-skill"),
            "description should reference skill name"
        );
    }

    #[test]
    fn list_skills_from_dir_skips_dirs_without_skill_md() {
        let tmp = tempfile::tempdir().unwrap();
        // A valid skill
        let s1 = tmp.path().join("skill-a");
        std::fs::create_dir_all(&s1).unwrap();
        std::fs::write(s1.join("SKILL.md"), "---\ndescription: A skill.\n---").unwrap();
        // A dir with no SKILL.md
        std::fs::create_dir_all(tmp.path().join("not-a-skill")).unwrap();

        let skills = list_skills_from_dir(tmp.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].0, "skill-a");
    }

    #[test]
    fn list_skills_from_dir_returns_empty_for_missing_dir() {
        let skills = list_skills_from_dir(std::path::Path::new("/nonexistent/path/to/skills"));
        assert!(skills.is_empty());
    }

    #[test]
    fn list_all_skills_sync_deduplicates_workspace_wins() {
        let global_tmp = tempfile::tempdir().unwrap();
        let workspace_tmp = tempfile::tempdir().unwrap();

        // Same skill name in both — workspace description should win
        for (dir, desc) in [
            (global_tmp.path(), "Global description"),
            (workspace_tmp.path(), "Workspace description"),
        ] {
            let skill_dir = dir.join("shared-skill");
            std::fs::create_dir_all(&skill_dir).unwrap();
            std::fs::write(
                skill_dir.join("SKILL.md"),
                format!("---\ndescription: {desc}.\n---"),
            )
            .unwrap();
        }

        // Additional global-only skill
        let global_only = global_tmp.path().join("global-only");
        std::fs::create_dir_all(&global_only).unwrap();
        std::fs::write(
            global_only.join("SKILL.md"),
            "---\ndescription: Global only.\n---",
        )
        .unwrap();

        let mut service = SkillService::new(None);
        service.global_skills_dir = global_tmp.path().to_path_buf();

        let skills = service.list_all_skills_sync(Some(workspace_tmp.path()));

        // Should have 2 unique skills
        assert_eq!(skills.len(), 2);
        // shared-skill should have the workspace description (listed first)
        let shared = skills.iter().find(|(n, _)| n == "shared-skill").unwrap();
        assert_eq!(shared.1, "Workspace description.");
        // global-only should also be present
        assert!(skills.iter().any(|(n, _)| n == "global-only"));
    }
}
