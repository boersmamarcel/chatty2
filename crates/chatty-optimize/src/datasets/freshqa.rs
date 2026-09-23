//! FreshQA loader (Vu et al., Findings of ACL 2024; FreshLLMs): questions
//! whose answers change over time, each with up to 10 acceptable answers.
//! Used as a one-off freshness check for `search_web` (AGE-517), not for
//! tuning.
//!
//! The upstream release is a Google Sheet (version dated in the file name);
//! `examples/search_eval_prepare.rs` converts its CSV export to the JSONL
//! this loader reads. Answers are as of that version's date, so
//! fast-changing items can go stale.

use super::{DatasetError, DatasetItem, load_json_or_jsonl};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FreshQaItem {
    pub id: String,
    pub question: String,
    /// `answer_0` first, then every non-empty `answer_1..=answer_9`.
    pub answers: Vec<String>,
    /// `never-changing`, `slow-changing` or `fast-changing`.
    pub fact_type: String,
    pub false_premise: bool,
    /// `DEV` or `TEST` (FreshQA's own split).
    pub split: String,
}

impl DatasetItem for FreshQaItem {
    fn id(&self) -> &str {
        &self.id
    }
}

pub fn load_freshqa(path: impl AsRef<Path>) -> Result<Vec<FreshQaItem>, DatasetError> {
    load_json_or_jsonl(path.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn loads_fixture() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/freshqa_sample.jsonl");
        let items = load_freshqa(path).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].answers, vec!["10", "10 schools", "ten"]);
        assert!(!items[0].false_premise);
    }
}
