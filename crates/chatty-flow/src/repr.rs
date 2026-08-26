//! Workflow representation trait (AGE-13).
//!
//! `WorkflowRepr` is **reserved**.

use crate::FlowError;

/// Swappable workflow backend (`IrRepr` first; Monty/Python later).
pub trait WorkflowRepr: Sized {
    fn mutate(&self, experience_json: &str) -> Result<Self, FlowError> {
        let _ = experience_json;
        todo!("human: WorkflowRepr — seam for IrRepr vs Monty code mode (AGE-13)")
    }
}

/// Placeholder so the module path exists for RESERVED.md.
pub struct UnimplementedRepr;

impl WorkflowRepr for UnimplementedRepr {
    fn mutate(&self, experience_json: &str) -> Result<Self, FlowError> {
        let _ = experience_json;
        todo!("human: WorkflowRepr — seam for IrRepr vs Monty code mode (AGE-13)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "human: WorkflowRepr")]
    fn workflow_repr_is_reserved() {
        let r = UnimplementedRepr;
        let _ = r.mutate("{}");
    }
}
