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
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
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

// ── Skill directories ────────────────────────────────────────────────────────

/// Folders that carry a `skills/` directory, in precedence order.
///
/// `.agents` is the cross-tool Agent Skills location (Codex, opencode, Cursor,
/// Warp and the `npx skills` installer all read or write it); `.claude` is
/// Claude Code's. Reading both means a skill installed for either tool shows
/// up in Chatty too.
const SKILL_ROOTS: &[&str] = &[".agents", ".claude"];

fn skill_dirs_in(dir: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    SKILL_ROOTS
        .iter()
        .map(move |root| dir.join(root).join("skills"))
}

/// Global skill directories, in precedence order: `~/.agents/skills/`, then
/// `~/.claude/skills/`. The same paths on every platform.
pub fn global_skill_dirs() -> Vec<PathBuf> {
    dirs::home_dir()
        .map(|home| skill_dirs_in(&home).collect())
        .unwrap_or_default()
}

/// Project skill directories for `workspace`, nearest first: `.agents/skills/`
/// and `.claude/skills/` in the workspace and in every parent up to the
/// enclosing git repository root, so a conversation rooted in a subfolder
/// still sees the repository's skills. Outside a git repository only the
/// workspace itself is searched.
pub fn workspace_skill_dirs(workspace: &Path) -> Vec<PathBuf> {
    let in_repo = workspace.ancestors().any(|d| d.join(".git").exists());
    let mut dirs = Vec::new();
    for dir in workspace.ancestors() {
        dirs.extend(skill_dirs_in(dir));
        if !in_repo || dir.join(".git").exists() {
            break;
        }
    }
    dirs
}

// ── SkillService ─────────────────────────────────────────────────────────────

/// Loads filesystem skills and scores them against a query, with on-disk
/// embedding caching to avoid redundant API calls.
///
/// ## Skill directories
/// Searched in this order; the first skill of a given name wins:
/// 1. **Project**: [`workspace_skill_dirs`] — `.agents/skills/` and
///    `.claude/skills/` from the workspace up to its git root
/// 2. **Global**: [`global_skill_dirs`] — `~/.agents/skills/` and
///    `~/.claude/skills/`
///
/// Each directory is scanned for immediate subdirectories (symlinks followed)
/// that contain a `SKILL.md` (or `skill.md`) file. The subdirectory name
/// becomes the skill name. Embeddings are cached as sidecar files inside each
/// skill subdirectory.
///
/// ## Scoring
/// When a `query_embedding` is provided skills are scored by cosine similarity
/// against their cached embeddings. If no cache exists the embedding is computed
/// via the service's `EmbeddingService` and written to disk for future calls.
/// Falls back to keyword overlap when no embedding is available.
#[derive(Clone)]
pub struct SkillService {
    global_skill_dirs: Vec<PathBuf>,
    embedding_service: Option<EmbeddingService>,
}

/// Synchronously list all available skills from the given directory.
///
/// Returns a list of `(name, description)` pairs for every skill subdirectory
/// that contains a `SKILL.md` or `skill.md` file.  Skills without a frontmatter
/// `description` field fall back to `"Skill: <name>"`.
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

/// Every skill in `dirs`, deduplicated by name so a skill in an earlier
/// directory shadows a same-named one in a later directory.
fn list_all_skills_from_dirs(dirs: &[(PathBuf, MemoryHitSource)]) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    dirs.iter()
        .flat_map(|(dir, _)| list_skills_from_dir(dir))
        .filter(|(name, _)| seen.insert(name.clone()))
        .collect()
}

impl SkillService {
    /// Create a new `SkillService`.
    ///
    /// `embedding_service` is optional; pass `None` to use keyword-only scoring.
    pub fn new(embedding_service: Option<EmbeddingService>) -> Self {
        Self {
            global_skill_dirs: global_skill_dirs(),
            embedding_service,
        }
    }

    /// Create a `SkillService` with custom global skill directories.
    #[cfg(test)]
    pub fn with_global_dirs(global_skill_dirs: Vec<PathBuf>) -> Self {
        Self {
            global_skill_dirs,
            embedding_service: None,
        }
    }

    /// Every directory searched for `workspace_dir`, in precedence order,
    /// tagged with where it came from.
    pub fn skill_dirs(&self, workspace_dir: Option<&Path>) -> Vec<(PathBuf, MemoryHitSource)> {
        let workspace = workspace_dir
            .map(workspace_skill_dirs)
            .unwrap_or_default()
            .into_iter()
            .map(|d| (d, MemoryHitSource::WorkspaceSkillFile));
        let global = self
            .global_skill_dirs
            .iter()
            .cloned()
            .map(|d| (d, MemoryHitSource::GlobalSkillFile));
        workspace.chain(global).collect()
    }

    /// Synchronously list all skills visible from `workspace_dir` (the
    /// workspace root, not a skills directory).
    ///
    /// Project skills are listed first, followed by global skills. Duplicate
    /// names are deduplicated (the first directory wins).
    ///
    /// This walks the filesystem on the calling thread. Callers on a
    /// latency-sensitive path (the desktop app builds its window on the main
    /// thread) want [`Self::list_all_skills`] instead.
    pub fn list_all_skills_sync(&self, workspace_dir: Option<&Path>) -> Vec<(String, String)> {
        list_all_skills_from_dirs(&self.skill_dirs(workspace_dir))
    }

    /// List all skills visible from `workspace_dir`, off the calling thread.
    ///
    /// Same result as [`Self::list_all_skills_sync`], but the directory walk
    /// runs on a blocking-pool thread so it cannot stall the caller. The
    /// desktop app populates its slash-command picker this way rather than
    /// scanning the filesystem while it is constructing the window (AGE-161).
    ///
    /// Takes the workspace directory by value because the walk outlives the
    /// call.
    pub async fn list_all_skills(&self, workspace_dir: Option<PathBuf>) -> Vec<(String, String)> {
        let service = self.clone();
        tokio::task::spawn_blocking(move || service.list_all_skills_sync(workspace_dir.as_deref()))
            .await
            .unwrap_or_else(|e| {
                warn!(error = ?e, "Skill listing task failed; reporting no skills");
                Vec::new()
            })
    }

    /// Load skill hits from every project and global skills directory.
    ///
    /// Skills are scored by cosine similarity (cached) or keyword overlap. A
    /// name found in more than one directory is reported once, from the first
    /// directory. The caller should sort and truncate the returned hits
    /// together with any persisted memory hits before injecting them into
    /// context.
    pub async fn load_hits(
        &self,
        query: &str,
        query_embedding: Option<&[f32]>,
        workspace_dir: Option<&Path>,
    ) -> Vec<MemoryHit> {
        let mut seen = HashSet::new();
        let mut hits = Vec::new();
        for (dir, source) in self.skill_dirs(workspace_dir) {
            for hit in self
                .load_from_dir(&dir, source, query, query_embedding)
                .await
            {
                if seen.insert(hit.title.clone()) {
                    hits.push(hit);
                }
            }
        }
        hits
    }

    /// Scan a single skills directory and return scored `MemoryHit` objects.
    async fn load_from_dir(
        &self,
        skills_dir: &Path,
        source: MemoryHitSource,
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
                frame_id: None,
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
        let skill_dir = tmp.path().join(".agents/skills/my-skill");
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();
        let content = "---\nname: my-skill\ndescription: Short description.\n---\n\n# Heading\n\nLong content that should NOT be in context.";
        tokio::fs::write(skill_dir.join("SKILL.md"), content)
            .await
            .unwrap();

        // Use an empty global dir so only the workspace skill is found.
        let empty_global = tempfile::tempdir().unwrap();
        let service = SkillService::with_global_dirs(vec![empty_global.path().to_path_buf()]);
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
        let workspace_skills = workspace_tmp.path().join(".agents/skills");

        // Same skill name in both — workspace description should win
        for (dir, desc) in [
            (global_tmp.path(), "Global description"),
            (workspace_skills.as_path(), "Workspace description"),
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

        let service = SkillService::with_global_dirs(vec![global_tmp.path().to_path_buf()]);

        let skills = service.list_all_skills_sync(Some(workspace_tmp.path()));

        // Should have 2 unique skills
        assert_eq!(skills.len(), 2);
        // shared-skill should have the workspace description (listed first)
        let shared = skills.iter().find(|(n, _)| n == "shared-skill").unwrap();
        assert_eq!(shared.1, "Workspace description.");
        // global-only should also be present
        assert!(skills.iter().any(|(n, _)| n == "global-only"));
    }

    /// The async listing exists to move the filesystem walk off the caller's
    /// thread (AGE-161), not to change what it finds: it must agree with the
    /// sync listing element for element, ordering included.
    #[tokio::test]
    async fn list_all_skills_matches_the_sync_listing() {
        let global_tmp = tempfile::tempdir().unwrap();
        let workspace_tmp = tempfile::tempdir().unwrap();
        let workspace_skills = workspace_tmp.path().join(".agents/skills");

        for (dir, name, desc) in [
            (global_tmp.path(), "shared-skill", "Global description"),
            (global_tmp.path(), "global-only", "Global only"),
            (
                workspace_skills.as_path(),
                "shared-skill",
                "Workspace description",
            ),
            (
                workspace_skills.as_path(),
                "workspace-only",
                "Workspace only",
            ),
        ] {
            let skill_dir = dir.join(name);
            std::fs::create_dir_all(&skill_dir).unwrap();
            std::fs::write(
                skill_dir.join("SKILL.md"),
                format!("---\ndescription: {desc}.\n---"),
            )
            .unwrap();
        }

        let service = SkillService::with_global_dirs(vec![global_tmp.path().to_path_buf()]);

        let sync = service.list_all_skills_sync(Some(workspace_tmp.path()));
        let asynchronous = service
            .list_all_skills(Some(workspace_tmp.path().to_path_buf()))
            .await;

        assert_eq!(sync, asynchronous);
        assert_eq!(sync.len(), 3, "shared-skill must be deduplicated");
    }
    fn write_skill(dir: &Path, name: &str, desc: &str) {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {desc}\n---\n# Body"),
        )
        .unwrap();
    }

    /// The open Agent Skills location and Claude Code's are both read, in
    /// the project and globally, and `.agents` wins a name clash.
    #[test]
    fn reads_agents_and_claude_dirs_in_project_and_global() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write_skill(&project.path().join(".agents/skills"), "proj-agents", "A.");
        write_skill(&project.path().join(".claude/skills"), "proj-claude", "C.");
        write_skill(
            &project.path().join(".agents/skills"),
            "clash",
            "From .agents.",
        );
        write_skill(
            &project.path().join(".claude/skills"),
            "clash",
            "From .claude.",
        );
        write_skill(&home.path().join(".agents/skills"), "global-agents", "GA.");
        write_skill(&home.path().join(".claude/skills"), "global-claude", "GC.");

        let service = SkillService::with_global_dirs(skill_dirs_in(home.path()).collect());
        let skills = service.list_all_skills_sync(Some(project.path()));
        let names: Vec<&str> = skills.iter().map(|(n, _)| n.as_str()).collect();

        assert_eq!(
            names,
            [
                "clash",
                "proj-agents",
                "proj-claude",
                "global-agents",
                "global-claude"
            ]
        );
        assert_eq!(skills[0].1, "From .agents.");
    }

    /// A workspace in a subfolder of a repository still sees the skills at
    /// the repository root, and the walk stops there.
    #[test]
    fn workspace_dirs_walk_up_to_the_git_root() {
        let outer = tempfile::tempdir().unwrap();
        let repo = outer.path().join("repo");
        let sub = repo.join("crates/app");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(&sub).unwrap();

        let dirs = workspace_skill_dirs(&sub);

        assert_eq!(dirs.first(), Some(&sub.join(".agents/skills")));
        assert!(dirs.contains(&repo.join(".agents/skills")));
        assert!(dirs.contains(&repo.join(".claude/skills")));
        assert!(!dirs.contains(&outer.path().join(".agents/skills")));
    }

    #[test]
    fn workspace_dirs_outside_a_repo_are_the_workspace_only() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = workspace_skill_dirs(tmp.path());
        // tempdir may itself sit under a git checkout; only assert the
        // no-repo case when it does not.
        if !tmp.path().ancestors().any(|d| d.join(".git").exists()) {
            assert_eq!(
                dirs,
                [
                    tmp.path().join(".agents/skills"),
                    tmp.path().join(".claude/skills")
                ]
            );
        }
    }

    /// `npx skills` installs into `.agents/skills` and symlinks the same
    /// skill into `.claude/skills`; it must reach the context once.
    #[tokio::test]
    async fn load_hits_reports_a_skill_in_two_dirs_once() {
        let project = tempfile::tempdir().unwrap();
        write_skill(&project.path().join(".agents/skills"), "twice", "Twice.");
        write_skill(&project.path().join(".claude/skills"), "twice", "Twice.");
        let empty_global = tempfile::tempdir().unwrap();
        let service = SkillService::with_global_dirs(vec![empty_global.path().to_path_buf()]);

        let hits = service.load_hits("twice", None, Some(project.path())).await;

        assert_eq!(hits.len(), 1);
    }
}
