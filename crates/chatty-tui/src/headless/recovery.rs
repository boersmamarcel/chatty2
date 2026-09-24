//! Tool-result recovery heuristics.
//!
//! Pure functions that classify tool-call outputs into "give up / parse exit
//! code" decisions used by the headless recovery loop in `mod.rs`. Stream
//! errors are the session's: `AgentSession::recovery_action` applies the
//! shared policy (`chatty_core::services::decide_recovery`) with the
//! session's own attempt bookkeeping (AGE-273).

use super::*;
use crate::engine::{ToolCallInfo, ToolCallState};

pub(super) fn tool_result_looks_failed(tool_call: &ToolCallInfo) -> bool {
    if matches!(tool_call.state, ToolCallState::Error) {
        return true;
    }
    let Some(output) = tool_call.output.as_deref() else {
        return false;
    };
    let lowered = output.to_ascii_lowercase();
    if lowered.contains("syntaxerror")
        || lowered.contains("traceback")
        || lowered.contains("toolset error")
        || lowered.contains("\"timed_out\": true")
        || lowered.contains("timed out")
    {
        return true;
    }
    parse_exit_code(output).is_some_and(|exit_code| exit_code != 0)
}

/// Whether a failed tool result is the harness's own sandbox/path policy
/// refusing the call (e.g. `read_file` outside the workspace). That is the
/// model learning where it may look, not a sign it is stuck, so it does not
/// count toward the failure budget.
pub(super) fn tool_result_is_policy_refusal(tool_call: &ToolCallInfo) -> bool {
    let Some(output) = tool_call.output.as_deref() else {
        return false;
    };
    let lowered = output.to_ascii_lowercase();
    lowered.contains("outside the workspace")
        || lowered.contains("access denied: path")
        || lowered.contains("access denied: glob")
        || lowered.contains("not allowed")
}

pub(super) fn parse_exit_code(output: &str) -> Option<i32> {
    parse_json_number_field(output, "exit_code").and_then(|value| i32::try_from(value).ok())
}
