use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::services::filesystem_service::FileSystemService;
use crate::tools::ToolError;

const DEFAULT_TOP_K: usize = 3;
const MAX_TOP_K: usize = 5;
const DEFAULT_MAX_FILES: usize = 30;
const MAX_FILES: usize = 80;
const DEFAULT_MAX_CHUNK_CHARS: usize = 900;
const MAX_CHUNK_CHARS: usize = 1_500;
const K1: f64 = 1.2;
const B: f64 = 0.75;

#[derive(Debug, Deserialize, Serialize)]
pub struct DocRetrieverArgs {
    pub query: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub top_k: Option<usize>,
    #[serde(default)]
    pub max_files: Option<usize>,
    #[serde(default)]
    pub max_chunk_chars: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct DocRetrieverOutput {
    pub query: String,
    pub root: String,
    pub files_indexed: usize,
    pub chunks_indexed: usize,
    pub results: Vec<DocRetrieverResult>,
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct DocRetrieverResult {
    pub path: String,
    pub title: String,
    pub start_line: usize,
    pub end_line: usize,
    pub score: f64,
    pub text: String,
}

#[derive(Clone)]
pub struct DocRetrieverTool {
    service: Arc<FileSystemService>,
}

impl DocRetrieverTool {
    pub fn new(service: Arc<FileSystemService>) -> Self {
        Self { service }
    }
}

impl Tool for DocRetrieverTool {
    const NAME: &'static str = "doc_retriever";
    type Error = ToolError;
    type Args = DocRetrieverArgs;
    type Output = DocRetrieverOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let top_k_description =
            format!("Number of chunks to return. Default: {DEFAULT_TOP_K}. Max: {MAX_TOP_K}.");
        let max_files_description = format!(
            "Maximum documentation files to index. Default: {DEFAULT_MAX_FILES}. Max: {MAX_FILES}."
        );
        let max_chunk_chars_description = format!(
            "Maximum text characters per returned chunk. Default: {DEFAULT_MAX_CHUNK_CHARS}. Max: {MAX_CHUNK_CHARS}."
        );
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Search local Markdown/text documentation with sparse BM25 retrieval. \
                          Use only when you need exact documentation rules, definitions, formulas, or field semantics after mapping the files. \
                          Do not use for merchant-specific facts, table values, or repeated exploration; use profile_data/query_data for those. \
                          Returns compact chunks with file path, section title, line span, score, and text."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Natural language or keyword query for the documentation."
                    },
                    "path": {
                        "type": "string",
                        "description": "File or directory to search, relative to workspace root. Defaults to workspace root."
                    },
                    "top_k": {
                        "type": "integer",
                        "description": top_k_description
                    },
                    "max_files": {
                        "type": "integer",
                        "description": max_files_description
                    },
                    "max_chunk_chars": {
                        "type": "integer",
                        "description": max_chunk_chars_description
                    }
                },
                "required": ["query"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let query = args.query.trim().to_string();
        if query.is_empty() {
            return Err(ToolError::OperationFailed(
                "doc_retriever query must not be empty".to_string(),
            ));
        }

        let requested_path = args.path.unwrap_or_else(|| ".".to_string());
        let root = self
            .service
            .resolve_path(&requested_path)
            .await
            .map_err(|e| ToolError::OperationFailed(e.to_string()))?;
        let workspace_root = self.service.workspace_root().to_path_buf();
        let top_k = args.top_k.unwrap_or(DEFAULT_TOP_K).clamp(1, MAX_TOP_K);
        let max_files = args
            .max_files
            .unwrap_or(DEFAULT_MAX_FILES)
            .clamp(1, MAX_FILES);
        let max_chunk_chars = args
            .max_chunk_chars
            .unwrap_or(DEFAULT_MAX_CHUNK_CHARS)
            .clamp(200, MAX_CHUNK_CHARS);
        let query_for_worker = query.clone();

        tokio::task::spawn_blocking(move || {
            retrieve_docs(
                &workspace_root,
                &root,
                &query_for_worker,
                top_k,
                max_files,
                max_chunk_chars,
            )
            .map_err(|e| ToolError::OperationFailed(e.to_string()))
        })
        .await
        .map_err(|e| ToolError::OperationFailed(format!("Task error: {e}")))?
        .map(|mut output| {
            output.query = query;
            output
        })
    }
}

#[derive(Debug)]
struct DocChunk {
    path: String,
    title: String,
    start_line: usize,
    end_line: usize,
    text: String,
    tokens: Vec<String>,
}

fn retrieve_docs(
    workspace_root: &Path,
    root: &Path,
    query: &str,
    top_k: usize,
    max_files: usize,
    max_chunk_chars: usize,
) -> anyhow::Result<DocRetrieverOutput> {
    if !root.exists() {
        anyhow::bail!("Path not found: {}", root.display());
    }

    let files = collect_doc_files(root, max_files + 1)?;
    let truncated = files.len() > max_files;
    let files = files.into_iter().take(max_files).collect::<Vec<_>>();
    let mut chunks = Vec::new();
    let mut notes = Vec::new();

    for path in &files {
        match chunk_document(workspace_root, path) {
            Ok(mut doc_chunks) => chunks.append(&mut doc_chunks),
            Err(e) => notes.push(format!(
                "{} skipped: {e}",
                relative_path(workspace_root, path)
            )),
        }
    }
    if truncated {
        notes.push(format!(
            "Documentation file listing truncated to {max_files} files; search a narrower path for more detail."
        ));
    }

    let results = bm25_rank(&chunks, query, top_k, max_chunk_chars);
    Ok(DocRetrieverOutput {
        query: String::new(),
        root: relative_path(workspace_root, root),
        files_indexed: files.len(),
        chunks_indexed: chunks.len(),
        results,
        notes,
    })
}

fn collect_doc_files(root: &Path, limit: usize) -> anyhow::Result<Vec<PathBuf>> {
    if root.is_file() {
        return Ok(is_doc_file(root)
            .then(|| root.to_path_buf())
            .into_iter()
            .collect());
    }
    if !root.is_dir() {
        anyhow::bail!("Path is not a file or directory: {}", root.display());
    }

    let mut files = Vec::new();
    let mut dirs = VecDeque::from([root.to_path_buf()]);
    while let Some(dir) = dirs.pop_front() {
        let mut children = std::fs::read_dir(&dir)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        children.sort();
        for path in children {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                dirs.push_back(path);
            } else if path.is_file() && is_doc_file(&path) {
                files.push(path);
                if files.len() >= limit {
                    return Ok(files);
                }
            }
        }
    }
    Ok(files)
}

fn is_doc_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "md" | "markdown" | "txt" | "rst"
    )
}

fn chunk_document(workspace_root: &Path, path: &Path) -> anyhow::Result<Vec<DocChunk>> {
    let text = std::fs::read_to_string(path)?;
    let lines = text.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        return Ok(Vec::new());
    }

    let mut chunks = Vec::new();
    let mut start_idx = 0usize;
    let mut title = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("document")
        .to_string();

    for (idx, line) in lines.iter().enumerate() {
        if let Some(heading) = markdown_heading(line) {
            if idx > start_idx {
                push_chunk(
                    workspace_root,
                    path,
                    &lines,
                    start_idx,
                    idx - 1,
                    &title,
                    &mut chunks,
                );
            }
            start_idx = idx;
            title = heading;
        }
    }
    push_chunk(
        workspace_root,
        path,
        &lines,
        start_idx,
        lines.len().saturating_sub(1),
        &title,
        &mut chunks,
    );
    Ok(chunks)
}

fn push_chunk(
    workspace_root: &Path,
    path: &Path,
    lines: &[&str],
    start_idx: usize,
    end_idx: usize,
    title: &str,
    chunks: &mut Vec<DocChunk>,
) {
    if start_idx > end_idx || start_idx >= lines.len() {
        return;
    }
    let end_idx = end_idx.min(lines.len().saturating_sub(1));
    let text = lines[start_idx..=end_idx].join("\n").trim().to_string();
    if text.is_empty() {
        return;
    }
    let token_text = format!("{title}\n{title}\n{text}");
    chunks.push(DocChunk {
        path: relative_path(workspace_root, path),
        title: title.chars().take(160).collect(),
        start_line: start_idx + 1,
        end_line: end_idx + 1,
        text,
        tokens: tokenize(&token_text),
    });
}

fn markdown_heading(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|ch| *ch == '#').count();
    if (1..=6).contains(&level) && trimmed.chars().nth(level) == Some(' ') {
        Some(trimmed[level + 1..].trim().chars().take(160).collect())
    } else {
        None
    }
}

fn bm25_rank(
    chunks: &[DocChunk],
    query: &str,
    top_k: usize,
    max_chunk_chars: usize,
) -> Vec<DocRetrieverResult> {
    if chunks.is_empty() {
        return Vec::new();
    }
    let query_terms = tokenize(query).into_iter().collect::<HashSet<_>>();
    if query_terms.is_empty() {
        return Vec::new();
    }

    let avg_len = chunks
        .iter()
        .map(|chunk| chunk.tokens.len() as f64)
        .sum::<f64>()
        / chunks.len() as f64;
    let mut doc_freq: HashMap<&str, usize> = HashMap::new();
    for chunk in chunks {
        let unique = chunk
            .tokens
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        for term in unique {
            *doc_freq.entry(term).or_default() += 1;
        }
    }

    let total_docs = chunks.len() as f64;
    let mut scored = chunks
        .iter()
        .filter_map(|chunk| {
            let mut term_freq: HashMap<&str, usize> = HashMap::new();
            for token in &chunk.tokens {
                *term_freq.entry(token.as_str()).or_default() += 1;
            }
            let doc_len = chunk.tokens.len() as f64;
            let mut score = 0.0;
            for term in &query_terms {
                let tf = *term_freq.get(term.as_str()).unwrap_or(&0) as f64;
                if tf == 0.0 {
                    continue;
                }
                let df = *doc_freq.get(term.as_str()).unwrap_or(&0) as f64;
                let idf = ((total_docs - df + 0.5) / (df + 0.5) + 1.0).ln();
                let denom = tf + K1 * (1.0 - B + B * doc_len / avg_len.max(1.0));
                score += idf * (tf * (K1 + 1.0)) / denom;
            }
            (score > 0.0).then_some((score, chunk))
        })
        .collect::<Vec<_>>();

    scored.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .partial_cmp(left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.start_line.cmp(&right.start_line))
    });
    scored
        .into_iter()
        .take(top_k)
        .map(|(score, chunk)| DocRetrieverResult {
            path: chunk.path.clone(),
            title: chunk.title.clone(),
            start_line: chunk.start_line,
            end_line: chunk.end_line,
            score: (score * 1000.0).round() / 1000.0,
            text: compact_text(&chunk.text, max_chunk_chars),
        })
        .collect()
}

fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            current.push(ch);
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn compact_text(text: &str, max_chars: usize) -> String {
    let normalized = text
        .replace(['\r', '\t'], " ")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    normalized
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>()
        + "..."
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .trim_start_matches('/')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig::tool::Tool;

    #[tokio::test]
    async fn retrieves_relevant_markdown_section() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("manual.md"),
            "# Manual\n\nIntro\n\n## Authorization Characteristics Indicator\nACI values A, B, C and E define transaction authorization quality.\n\n## Fraud Fees\nFraud-related fees use monthly fraud level buckets and transaction volume.\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("readme.txt"),
            "Payments contain merchants, amounts, and countries.",
        )
        .unwrap();

        let service = Arc::new(
            FileSystemService::new(dir.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = DocRetrieverTool::new(service);
        let output = tool
            .call(DocRetrieverArgs {
                query: "monthly fraud fee buckets".to_string(),
                path: None,
                top_k: Some(2),
                max_files: None,
                max_chunk_chars: Some(500),
            })
            .await
            .unwrap();

        assert_eq!(output.files_indexed, 2);
        assert!(!output.results.is_empty());
        assert_eq!(output.results[0].title, "Fraud Fees");
        assert_eq!(output.results[0].path, "manual.md");
        assert!(output.results[0].text.contains("monthly fraud level"));
    }
}
