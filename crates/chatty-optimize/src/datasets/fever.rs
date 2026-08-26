//! FEVER fact-verification loader.

use super::{DatasetError, DatasetItem, load_json_or_jsonl};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One FEVER claim with a gold label (`SUPPORTS` / `REFUTES` / `NOT ENOUGH INFO`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeverItem {
    pub id: String,
    pub claim: String,
    pub label: String,
}

impl DatasetItem for FeverItem {
    fn id(&self) -> &str {
        &self.id
    }
}

pub fn load_fever(path: impl AsRef<Path>) -> Result<Vec<FeverItem>, DatasetError> {
    load_json_or_jsonl(path.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn loads_fixture() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/fever_sample.jsonl");
        let items = load_fever(path).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].label, "SUPPORTS");
    }
}
