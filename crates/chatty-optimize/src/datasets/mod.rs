//! Dataset loaders with deterministic seeded splits.

mod fever;
mod gsm8k;
mod hotpotqa;
mod humaneval;
mod split;

pub use fever::{FeverItem, load_fever};
pub use gsm8k::{Gsm8kItem, load_gsm8k};
pub use hotpotqa::{HotpotQaItem, load_hotpotqa};
pub use humaneval::{HumanEvalItem, load_humaneval};
pub use split::{Split, SplitParts, SplitSpec, split_items};

use serde::de::DeserializeOwned;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use thiserror::Error;

/// Common error for dataset I/O and parse failures.
#[derive(Debug, Error)]
pub enum DatasetError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("empty dataset at {0}")]
    Empty(String),
    #[error(
        "split fractions must be positive and sum to <= 1.0 (got train={train}, val={val}, test={test})"
    )]
    InvalidSplit { train: f64, val: f64, test: f64 },
}

/// Minimal shared view of a scored eval item.
pub trait DatasetItem {
    fn id(&self) -> &str;
}

/// Load newline-delimited JSON (JSONL) or a JSON array from `path`.
pub(crate) fn load_json_or_jsonl<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>, DatasetError> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut first = String::new();
    let bytes = reader.read_line(&mut first)?;
    if bytes == 0 {
        return Err(DatasetError::Empty(path.display().to_string()));
    }
    let trimmed = first.trim_start();
    if trimmed.starts_with('[') {
        // Rewind by reopening — BufReader already consumed the first line.
        let all = std::fs::read_to_string(path)?;
        let items: Vec<T> = serde_json::from_str(&all)?;
        if items.is_empty() {
            return Err(DatasetError::Empty(path.display().to_string()));
        }
        return Ok(items);
    }

    let mut items = Vec::new();
    items.push(serde_json::from_str(first.trim())?);
    for line in reader.lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        items.push(serde_json::from_str(line)?);
    }
    if items.is_empty() {
        return Err(DatasetError::Empty(path.display().to_string()));
    }
    Ok(items)
}
