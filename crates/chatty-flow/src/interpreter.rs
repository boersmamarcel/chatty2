//! IrRepr interpreter scaffolding (agent-owned). Reserved types live in `repr` / `ir`.

use crate::FlowError;
use crate::ir::WfNode;

/// Execute a validated IR graph. Body expands once `WfNode` is defined.
pub fn interpret(_nodes: &[WfNode], _input: &str) -> Result<String, FlowError> {
    Err(FlowError::NotReady(
        "interpreter waits on human WfNode / WorkflowRepr",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpreter_reports_not_ready() {
        let err = interpret(&[], "hi").unwrap_err();
        assert!(matches!(err, FlowError::NotReady(_)));
    }
}
