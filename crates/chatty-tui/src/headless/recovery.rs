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
///
/// Only a tool's own error counts, never a command's output: `shell_execute`
/// and `execute_code` print whatever the program printed, and a Python
/// `ValueError: negative dimensions are not allowed` or an HTTP `405 Method
/// Not Allowed` page is a real failure. The phrases are the path validator's
/// and the output-path check's, not a bare "not allowed".
pub(super) fn tool_result_is_policy_refusal(tool_call: &ToolCallInfo) -> bool {
    if !matches!(tool_call.state, ToolCallState::Error)
        || COMMAND_TOOLS.contains(&tool_call.name.as_str())
    {
        return false;
    }
    let Some(output) = tool_call.output.as_deref() else {
        return false;
    };
    let lowered = output.to_ascii_lowercase();
    lowered.contains("access denied: path")
        || lowered.contains("access denied: glob")
        || lowered.contains("is outside the workspace")
        || lowered.contains("resolves outside the workspace")
        || lowered.contains("path not allowed:")
}

pub(super) fn parse_exit_code(output: &str) -> Option<i32> {
    parse_json_number_field(output, "exit_code").and_then(|value| i32::try_from(value).ok())
}
