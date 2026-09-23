//! OpenAI SimpleQA loader (AGE-515): short fact-seeking questions with one
//! verifiable answer, used to score `search_web` on its own.
//!
//! The upstream file is `simple_qa_test_set.csv` (columns `metadata`,
//! `problem`, `answer`); `examples/search_eval_prepare.rs` converts it to the
//! JSONL this loader reads, with `id` = CSV row index.

use super::{DatasetError, DatasetItem, load_json_or_jsonl};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SimpleQaItem {
    pub id: String,
    pub question: String,
    pub answer: String,
    /// `metadata.topic`, e.g. "Science and technology". Sampling stratum.
    pub topic: String,
}

impl DatasetItem for SimpleQaItem {
    fn id(&self) -> &str {
        &self.id
    }
}

pub fn load_simpleqa(path: impl AsRef<Path>) -> Result<Vec<SimpleQaItem>, DatasetError> {
    load_json_or_jsonl(path.as_ref())
}

/// Pull `topic` out of SimpleQA's `metadata` column, which is a Python dict
/// literal (`{'topic': 'Art', 'answer_type': ...}`), not JSON.
pub fn parse_simpleqa_topic(metadata: &str) -> Option<String> {
    let rest = &metadata[metadata.find("'topic':")? + "'topic':".len()..];
    let rest = rest.trim_start();
    let quote = rest.chars().next().filter(|c| *c == '\'' || *c == '"')?;
    let body = &rest[1..];
    Some(body[..body.find(quote)?].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn loads_fixture() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/simpleqa_sample.jsonl");
        let items = load_simpleqa(path).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].id, "0");
        assert_eq!(items[0].answer, "Michio Sugeno");
        assert_eq!(items[0].topic, "Science and technology");
    }

    #[test]
    fn parses_topic_from_python_dict_literal() {
        let meta =
            "{'topic': 'Science and technology', 'answer_type': 'Person', 'urls': ['https://a']}";
        assert_eq!(
            parse_simpleqa_topic(meta).as_deref(),
            Some("Science and technology")
        );
        assert_eq!(parse_simpleqa_topic("{'answer_type': 'Person'}"), None);
    }
}
