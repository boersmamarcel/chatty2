//! HotpotQA multi-hop QA loader.

use super::{DatasetError, DatasetItem, load_json_or_jsonl};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One HotpotQA question with supporting-fact titles (gold docs).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HotpotQaItem {
    pub id: String,
    pub question: String,
    pub answer: String,
    /// Gold supporting document titles (for per-hop feedback once FeedbackFn lands).
    #[serde(default)]
    pub supporting_titles: Vec<String>,
}

impl DatasetItem for HotpotQaItem {
    fn id(&self) -> &str {
        &self.id
    }
}

/// Load HotpotQA items from JSON array or JSONL at `path`.
pub fn load_hotpotqa(path: impl AsRef<Path>) -> Result<Vec<HotpotQaItem>, DatasetError> {
    load_json_or_jsonl(path.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/hotpotqa_sample.jsonl")
    }

    #[test]
    fn loads_fixture() {
        let items = load_hotpotqa(fixture()).unwrap();
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].id, "hp-1");
        assert!(!items[0].supporting_titles.is_empty());
    }
}
