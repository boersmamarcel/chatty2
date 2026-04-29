use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use tracing::warn;

use super::save_skill_tool::SKILL_TITLE_PREFIX;
use crate::services::embedding_service::EmbeddingService;
use crate::services::memory_service::{MemoryHit, MemoryHitSource, MemoryService};
use crate::services::skill_service::SkillService;
use crate::tools::ToolError;

/// Arguments for the search_memory tool
#[derive(Deserialize, Serialize)]
pub struct SearchMemoryToolArgs {
    /// Natural language search query
    pub query: String,
    /// Maximum number of results to return (default: 5)
    #[serde(default)]
    pub top_k: Option<usize>,
}

/// Output from the search_memory tool
#[derive(Debug, Serialize)]
pub struct SearchMemoryToolOutput {
    pub results: Vec<MemoryHit>,
}

/// Tool that allows the agent to search its persistent memory and filesystem skills.
///
/// Searches across all previously stored memories using full-text keyword search
/// (BM25 ranking) and optionally scans workspace/global SKILL.md files. Queries match
/// on exact words, so use specific keywords.
#[derive(Clone)]
pub struct SearchMemoryTool {
    memory_service: MemoryService,
    embedding_service: Option<EmbeddingService>,
    /// Optional skill service for filesystem skill discovery.
    skill_service: Option<SkillService>,
    workspace_skills_dir: Option<PathBuf>,
}

impl SearchMemoryTool {
    pub fn new(
        memory_service: MemoryService,
        embedding_service: Option<EmbeddingService>,
        skill_service: Option<SkillService>,
        workspace_skills_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            memory_service,
            embedding_service,
            skill_service,
            workspace_skills_dir,
        }
    }
}

impl Tool for SearchMemoryTool {
    const NAME: &'static str = "search_memory";
    type Error = ToolError;
    type Args = SearchMemoryToolArgs;
    type Output = SearchMemoryToolOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let (description, query_description) = if self.embedding_service.is_some() {
            (
                "Search persistent memory and available skills for previously stored information. \
                 Uses hybrid search: keyword matching (BM25) + semantic similarity. \
                 Also scans filesystem SKILL.md files so you can discover workspace/global skills. \
                 You can use natural language queries — searching 'fruits' will find \
                 memories about bananas, apples, etc.",
                "Natural language query describing what you want to recall or discover. \
                 Both specific keywords and conceptual descriptions work. \
                 Example: 'food preferences' will find memories about specific foods. \
                     Example: 'deployment' may surface a saved skill.",
            )
        } else {
            (
                "Search persistent memory and available skills for previously stored information. \
                 Use this when you need to recall facts, decisions, user preferences, \
                 or context from past conversations, or when you want to discover if a \
                 reusable procedure (skill) exists for a task. Uses keyword matching (BM25), \
                 so include specific words that are likely in the stored memory.",
                "Keyword query describing what you want to recall or discover. \
                 Use concrete nouns and terms likely present in stored memories. \
                 Example: 'bananas fruit preference' rather than 'what foods does the user like'.",
            )
        };

        ToolDefinition {
            name: "search_memory".to_string(),
            description: description.to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": query_description
                    },
                    "top_k": {
                        "type": "integer",
                        "description": "Maximum number of results to return (1-20). Defaults to 5."
                    }
                },
                "required": ["query", "top_k"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let top_k = args.top_k.unwrap_or(5);

        // BM25 lexical search (always runs)
        let lex_results = self
            .memory_service
            .search(&args.query, Some(top_k))
            .await
            .map_err(|e| ToolError::OperationFailed(e.to_string()))?;

        // Vector search (if embedding service is available)
        let mut query_embedding_opt: Option<Vec<f32>> = None;
        let vec_results = if let Some(ref embed_svc) = self.embedding_service {
            match embed_svc.embed(&args.query).await {
                Ok(embedding) => {
                    query_embedding_opt = Some(embedding.clone());
                    self
                        .memory_service
                        .search_vec(embedding, Some(top_k))
                        .await
                        .unwrap_or_else(|e| {
                            warn!(error = ?e, "Vector search failed, using BM25 only");
                            Vec::new()
                        })
                }
                Err(e) => {
                    warn!(error = ?e, "Query embedding failed, using BM25 only");
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        let memory_results = merge_search_results(lex_results, vec_results, top_k);

        // Scan filesystem skills (if skill service is available)
        let skill_results: Vec<MemoryHit> = if let Some(ref skill_svc) = self.skill_service {
            skill_svc
                .load_hits(&args.query, query_embedding_opt.as_deref(), self.workspace_skills_dir.as_deref())
                .await
        } else {
            Vec::new()
        };

        let results = select_context_hits(memory_results, skill_results, top_k);

        Ok(SearchMemoryToolOutput { results })
    }
}

/// Build a context block string from memory hits, partitioned into facts and skills.
///
/// Hits whose title starts with `SKILL_TITLE_PREFIX` are formatted in a separate
/// "skills" block showing only a short description per skill. Filesystem skills
/// can be expanded with `read_skill`; memory-backed skills should be revisited
/// with `search_memory`.
/// Returns `None` when there are no hits to display.
pub fn build_memory_context_block(hits: Vec<MemoryHit>) -> Option<String> {
    if hits.is_empty() {
        return None;
    }

    let (skill_hits, fact_hits): (Vec<_>, Vec<_>) = hits.into_iter().partition(|h| {
        h.title
            .as_deref()
            .map(|t| t.starts_with(SKILL_TITLE_PREFIX))
            .unwrap_or(false)
    });

    let mut block = String::new();

    if !fact_hits.is_empty() {
        block.push_str("[Relevant memories from past conversations]\n");
        for hit in &fact_hits {
            if let Some(ref title) = hit.title {
                block.push_str(&format!("- {}: {}\n", title, hit.text));
            } else {
                block.push_str(&format!("- {}\n", hit.text));
            }
        }
        block.push_str("[End of memories]\n\n");
    }

    if !skill_hits.is_empty() {
        block.push_str("[Relevant skills available]\n");
        block.push_str(
            "For filesystem skills, call `read_skill` with the skill name to get full instructions. For memory-backed skills, use `search_memory` to recall the saved procedure.\n",
        );
        block.push_str(
            "For Python-oriented skills, prefer `uv` for shell package management, or use `execute_code` when you want isolated sandbox execution.\n",
        );
        for hit in &skill_hits {
            let display_name = hit
                .title
                .as_deref()
                .map(|t| t.trim_start_matches(SKILL_TITLE_PREFIX))
                .unwrap_or("unnamed skill");
            let source_hint = match hit.source {
                Some(MemoryHitSource::Memory) => "memory-backed; use `search_memory`",
                Some(MemoryHitSource::WorkspaceSkillFile) => {
                    "workspace skill file; call `read_skill`"
                }
                Some(MemoryHitSource::GlobalSkillFile) => "global skill file; call `read_skill`",
                None => "source unknown",
            };
            let description = skill_description_line(&hit.text);
            block.push_str(&format!(
                "- \"{}\" [{}]: {}\n",
                display_name, source_hint, description
            ));
        }
        block.push_str("[End of skills]\n\n");
    }

    Some(block)
}

/// Extract a single-line description from a skill's stored text.
///
/// For filesystem skills the text is already just the description string.
/// For memory-stored skills (created via `save_skill`) the format is:
/// `Description: <desc>\n1. first step\n…` — strip the prefix and take the first line.
fn skill_description_line(text: &str) -> &str {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix("Description:") {
        // Take only the first line of the description value
        rest.trim().lines().next().unwrap_or("")
    } else {
        // Already a plain description (filesystem skill)
        text.lines().next().unwrap_or(text)
    }
}

/// Merge BM25 and vector search results, deduplicating by text content.
///
/// For duplicates (same text from both sources), keeps the higher score.
/// Returns up to `limit` results sorted by descending score.
pub fn merge_search_results(
    lex_results: Vec<MemoryHit>,
    vec_results: Vec<MemoryHit>,
    limit: usize,
) -> Vec<MemoryHit> {
    if vec_results.is_empty() {
        return lex_results;
    }
    if lex_results.is_empty() {
        return vec_results.into_iter().take(limit).collect();
    }

    // Deduplicate by text content, keeping the higher score
    let mut seen_texts = HashSet::new();
    let mut merged: Vec<MemoryHit> = Vec::with_capacity(lex_results.len() + vec_results.len());

    // Add all results, tracking seen texts
    for hit in lex_results.into_iter().chain(vec_results.into_iter()) {
        // Use first 200 chars as dedup key to avoid expensive full-text comparison
        let key = hit.text.chars().take(200).collect::<String>();
        if seen_texts.contains(&key) {
            // Update score if this duplicate has a higher score
            if let Some(existing) = merged
                .iter_mut()
                .find(|h| h.text.chars().take(200).collect::<String>() == key)
                && hit.score > existing.score
            {
                existing.score = hit.score;
                if existing.source.is_none() {
                    existing.source = hit.source;
                }
            }
        } else {
            seen_texts.insert(key);
            merged.push(hit);
        }
    }

    // Sort by descending score and take top `limit`
    merged.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    merged.truncate(limit);
    merged
}

fn is_skill_hit(hit: &MemoryHit) -> bool {
    hit.title
        .as_deref()
        .map(|t| t.starts_with(SKILL_TITLE_PREFIX))
        .unwrap_or(false)
}

fn sort_by_score_desc(hits: &mut [MemoryHit]) {
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

pub fn select_context_hits(
    merged_memory_hits: Vec<MemoryHit>,
    filesystem_skill_hits: Vec<MemoryHit>,
    limit: usize,
) -> Vec<MemoryHit> {
    let (mut skill_hits, mut fact_hits): (Vec<_>, Vec<_>) =
        merged_memory_hits.into_iter().partition(is_skill_hit);
    skill_hits.extend(filesystem_skill_hits);

    sort_by_score_desc(&mut fact_hits);
    sort_by_score_desc(&mut skill_hits);

    let mut selected = Vec::with_capacity(limit);
    let prioritized_fact_count = fact_hits.len().min(limit.min(3));
    let prioritized_skill_count = if fact_hits.is_empty() {
        limit.min(skill_hits.len())
    } else {
        (limit - prioritized_fact_count).min(skill_hits.len().min(2))
    };

    let mut remaining_facts = fact_hits.into_iter();
    let mut remaining_skills = skill_hits.into_iter();

    selected.extend(remaining_facts.by_ref().take(prioritized_fact_count));
    selected.extend(remaining_skills.by_ref().take(prioritized_skill_count));

    if selected.len() < limit {
        selected.extend(remaining_facts.by_ref().take(limit - selected.len()));
    }
    if selected.len() < limit {
        selected.extend(remaining_skills.by_ref().take(limit - selected.len()));
    }

    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::memory_service::MemoryHit;

    fn fact(title: &str, score: f32) -> MemoryHit {
        MemoryHit {
            text: format!("fact:{title}"),
            title: Some(title.to_string()),
            score,
            source: Some(MemoryHitSource::Memory),
            frame_id: None,
        }
    }

    fn saved_skill(name: &str, score: f32) -> MemoryHit {
        MemoryHit {
            text: format!("Description: {name}"),
            title: Some(format!("{SKILL_TITLE_PREFIX}{name}")),
            score,
            source: Some(MemoryHitSource::Memory),
            frame_id: None,
        }
    }

    fn file_skill(name: &str, score: f32) -> MemoryHit {
        MemoryHit {
            text: format!("# {name}"),
            title: Some(format!("{SKILL_TITLE_PREFIX}{name}")),
            score,
            source: Some(MemoryHitSource::WorkspaceSkillFile),
            frame_id: None,
        }
    }

    #[test]
    fn select_context_hits_prioritizes_facts_before_extra_skills() {
        let selected = select_context_hits(
            vec![
                fact("Fact A", 0.91),
                fact("Fact B", 0.87),
                fact("Fact C", 0.81),
                fact("Fact D", 0.75),
                saved_skill("Saved skill", 0.99),
            ],
            vec![
                file_skill("File skill", 0.95),
                file_skill("File skill 2", 0.93),
            ],
            5,
        );

        let titles: Vec<_> = selected
            .iter()
            .map(|hit| hit.title.clone().unwrap_or_default())
            .collect();
        assert_eq!(
            titles,
            vec![
                "Fact A",
                "Fact B",
                "Fact C",
                "[SKILL] Saved skill",
                "[SKILL] File skill"
            ]
        );
    }

    #[test]
    fn build_memory_context_block_includes_skill_source_hints() {
        let block =
            build_memory_context_block(vec![saved_skill("deploy", 0.9), file_skill("lint", 0.8)])
                .expect("context block");

        assert!(block.contains("memory-backed; use `search_memory`"));
        assert!(block.contains("workspace skill file; call `read_skill`"));
        assert!(block.contains("prefer `uv` for shell package management"));
    }

    #[test]
    fn context_block_extracts_description_from_memory_skill() {
        // Memory-stored skills have "Description: <desc>\n1. step…" format
        let hits = vec![saved_skill("deploy", 0.9)];
        let block = build_memory_context_block(hits).unwrap();
        assert!(block.contains("\"deploy\" [memory-backed; use `search_memory`]: deploy"));
        // Steps should not appear in the context block
        assert!(!block.contains("1. Build."));
    }

    #[test]
    fn context_block_still_shows_facts() {
        let hits = vec![fact("user pref", 0.8)];
        let block = build_memory_context_block(hits).unwrap();
        assert!(block.contains("[Relevant memories from past conversations]"));
        assert!(block.contains("fact:user pref"));
    }

    #[test]
    fn skill_description_line_strips_prefix_from_memory_format() {
        let text = "Description: Short desc.\n1. Step one.\n2. Step two.";
        assert_eq!(skill_description_line(text), "Short desc.");
    }

    #[test]
    fn skill_description_line_passthrough_for_plain_text() {
        // Filesystem skill already has just a description
        let text = "Runs the full build pipeline.";
        assert_eq!(
            skill_description_line(text),
            "Runs the full build pipeline."
        );
    }
}