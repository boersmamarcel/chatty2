//! SQL rewriting and quoting helpers for the data-query tool.
//!
//! Pure functions only — no I/O, no globals. The DuckDB connection is
//! threaded through; these helpers just rewrite query strings or quote
//! identifiers.

use std::path::{Path, PathBuf};

use super::DataQueryError;

pub(super) fn escape_sql_string(value: &str) -> String {
    value.replace('\'', "''")
}

pub(super) fn rewrite_query_file_literals(
    sql: &str,
    workspace_root: &Path,
) -> Result<String, DataQueryError> {
    let mut rewritten = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '\'' {
            rewritten.push(ch);
            continue;
        }

        let mut raw_literal = String::new();
        let mut original_literal = String::from("'");

        loop {
            let Some(next) = chars.next() else {
                return Err(DataQueryError::QueryFailed(
                    "Query contains an unterminated string literal".to_string(),
                ));
            };

            original_literal.push(next);
            if next == '\'' {
                if chars.peek() == Some(&'\'') {
                    chars.next();
                    original_literal.push('\'');
                    raw_literal.push('\'');
                    continue;
                }
                break;
            }

            raw_literal.push(next);
        }

        if let Some(path_literal) = resolve_workspace_file_literal(&raw_literal, workspace_root)? {
            rewritten.push('\'');
            rewritten.push_str(&escape_sql_string(&path_literal));
            rewritten.push('\'');
        } else {
            rewritten.push_str(&original_literal);
        }
    }

    Ok(rewritten)
}

pub(super) fn resolve_workspace_file_literal(
    literal: &str,
    workspace_root: &Path,
) -> Result<Option<String>, DataQueryError> {
    if !looks_like_file_literal(literal) {
        return Ok(None);
    }

    let requested = Path::new(literal);
    let candidate = if requested.is_absolute() {
        PathBuf::from(requested)
    } else {
        workspace_root.join(requested)
    };
    let resolved = resolve_literal_path(&candidate, workspace_root, literal)?;

    Ok(Some(resolved.to_string_lossy().to_string()))
}

pub(super) fn looks_like_file_literal(literal: &str) -> bool {
    let trimmed = literal.trim();
    if trimmed.is_empty() || trimmed.contains('\n') {
        return false;
    }
    // A URL is not a workspace path: it is left to DuckDB, which reads it
    // in place when HTTP reads are configured (AGE-1652) and refuses it
    // otherwise.
    if is_http_url(trimmed) {
        return false;
    }

    trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.starts_with('.')
        || trimmed.contains('*')
        || trimmed.contains('?')
        || trimmed
            .rsplit_once('.')
            .map(|(_, ext)| {
                matches!(
                    ext.to_ascii_lowercase().as_str(),
                    "csv" | "tsv" | "parquet" | "json" | "jsonl" | "ndjson" | "duckdb" | "db"
                )
            })
            .unwrap_or(false)
}

pub(super) fn resolve_literal_path(
    candidate: &Path,
    workspace_root: &Path,
    original_literal: &str,
) -> Result<PathBuf, DataQueryError> {
    let resolved = if candidate.exists() && !has_glob_pattern(candidate) {
        std::fs::canonicalize(candidate).map_err(|e| {
            DataQueryError::PathNotAllowed(format!(
                "Failed to resolve path '{original_literal}': {e}"
            ))
        })?
    } else {
        let parent = candidate.parent().unwrap_or(workspace_root);
        let canonical_parent = std::fs::canonicalize(parent).map_err(|e| {
            DataQueryError::PathNotAllowed(format!(
                "Failed to resolve path '{original_literal}': {e}"
            ))
        })?;

        let Some(file_name) = candidate.file_name() else {
            return Err(DataQueryError::PathNotAllowed(format!(
                "Path '{original_literal}' is not a file path"
            )));
        };

        canonical_parent.join(file_name)
    };

    if !resolved.starts_with(workspace_root) {
        return Err(DataQueryError::PathNotAllowed(format!(
            "Access denied: path '{original_literal}' is outside the workspace root"
        )));
    }

    Ok(resolved)
}

/// `http://` or `https://`, case-insensitively.
pub(super) fn is_http_url(literal: &str) -> bool {
    let lower = literal.get(..8).map(str::to_ascii_lowercase);
    lower
        .as_deref()
        .is_some_and(|l| l.starts_with("http://") || l.starts_with("https://"))
}

pub(super) fn has_glob_pattern(path: &Path) -> bool {
    path.to_string_lossy()
        .chars()
        .any(|ch| matches!(ch, '*' | '?' | '[' | ']'))
}
