//! IR node enum (AGE-13).
//!
//! `WfNode` is **reserved** — what IrRepr can express decides the week-6 kill criterion.

use crate::FlowError;

/// Node kinds in the serde DAG (Cond / Repeat / Ensemble / …).
///
/// Left as a stub enum; variants are filled by the human with the reserved decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WfNode {
    /// Placeholder until the human defines the real variants.
    Reserved,
}

impl WfNode {
    pub fn validate(&self) -> Result<(), FlowError> {
        todo!("human: WfNode — IR expressivity / Cond-Repeat-Ensemble (AGE-13)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "human: WfNode")]
    fn wf_node_validate_is_reserved() {
        let _ = WfNode::Reserved.validate();
    }
}
