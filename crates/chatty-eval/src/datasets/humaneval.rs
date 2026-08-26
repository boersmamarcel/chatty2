//! HumanEval coding problems loader (pass@1 evaluated via chatty-core sandbox later).

use super::{DatasetError, DatasetItem, load_json_or_jsonl};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HumanEvalItem {
    pub task_id: String,
    pub prompt: String,
    pub entry_point: String,
    pub canonical_solution: String,
    pub test: String,
}

impl DatasetItem for HumanEvalItem {
    fn id(&self) -> &str {
        &self.task_id
    }
}

pub fn load_humaneval(path: impl AsRef<Path>) -> Result<Vec<HumanEvalItem>, DatasetError> {
    load_json_or_jsonl(path.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn loads_fixture() {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/humaneval_sample.jsonl");
        let items = load_humaneval(path).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].entry_point, "has_close_elements");
    }
}
