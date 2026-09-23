//! Google FRAMES loader (AGE-516): multi-hop questions, each with the gold
//! Wikipedia pages needed to answer it.
//!
//! The upstream file is `google/frames-benchmark` `test.tsv` (824 rows);
//! `examples/search_eval_prepare.rs` converts it to the JSONL this loader
//! reads. Upstream quirks handled there: `wiki_links` is a stringified Python
//! list and `reasoning_types` is `|`-separated.

use super::{DatasetError, DatasetItem, load_json_or_jsonl};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FramesItem {
    pub id: String,
    pub prompt: String,
    pub answer: String,
    pub wiki_links: Vec<String>,
    /// E.g. `["Tabular reasoning", "Multiple constraints"]`; the first entry
    /// is the sampling stratum.
    pub reasoning_types: Vec<String>,
}

impl FramesItem {
    pub fn primary_reasoning_type(&self) -> &str {
        self.reasoning_types.first().map_or("", String::as_str)
    }
}

impl DatasetItem for FramesItem {
    fn id(&self) -> &str {
        &self.id
    }
}

pub fn load_frames(path: impl AsRef<Path>) -> Result<Vec<FramesItem>, DatasetError> {
    load_json_or_jsonl(path.as_ref())
}

/// Parse a stringified Python list of strings (`"['a', 'b']"`).
pub fn parse_py_str_list(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\'' && c != '"' {
            continue;
        }
        let mut item = String::new();
        for d in chars.by_ref() {
            if d == c {
                break;
            }
            item.push(d);
        }
        out.push(item);
    }
    out
}

/// Split FRAMES' `reasoning_types` column (`"Tabular reasoning | Multiple constraints"`).
pub fn parse_reasoning_types(s: &str) -> Vec<String> {
    s.split('|')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn loads_fixture() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/frames_sample.jsonl");
        let items = load_frames(path).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].wiki_links.len(), 5);
        assert_eq!(items[1].primary_reasoning_type(), "Numerical reasoning");
    }

    #[test]
    fn parses_stringified_list() {
        assert_eq!(
            parse_py_str_list(
                "['https://en.wikipedia.org/wiki/A', 'https://en.wikipedia.org/wiki/B']"
            ),
            vec![
                "https://en.wikipedia.org/wiki/A",
                "https://en.wikipedia.org/wiki/B"
            ]
        );
        assert!(parse_py_str_list("[]").is_empty());
    }

    #[test]
    fn splits_reasoning_types() {
        assert_eq!(
            parse_reasoning_types("Tabular reasoning | Multiple constraints"),
            vec!["Tabular reasoning", "Multiple constraints"]
        );
    }
}
