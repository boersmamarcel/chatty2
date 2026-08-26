//! GSM8K grade-school math loader.

use super::{DatasetError, DatasetItem, load_json_or_jsonl};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Gsm8kItem {
    pub id: String,
    pub question: String,
    /// Final numeric answer as a string (normalized later by scorers).
    pub answer: String,
}

impl DatasetItem for Gsm8kItem {
    fn id(&self) -> &str {
        &self.id
    }
}

pub fn load_gsm8k(path: impl AsRef<Path>) -> Result<Vec<Gsm8kItem>, DatasetError> {
    load_json_or_jsonl(path.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn loads_fixture() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/gsm8k_sample.jsonl");
        let items = load_gsm8k(path).unwrap();
        assert_eq!(items.len(), 3);
    }
}
