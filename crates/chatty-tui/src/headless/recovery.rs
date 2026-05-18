//! Stream-error and tool-result recovery heuristics.
//!
//! Pure functions that classify provider errors and tool-call outputs into
//! "retry / give up / parse exit code" decisions used by the headless
//! recovery loop in `mod.rs`.

use super::*;
use crate::engine::{ToolCallInfo, ToolCallState};

pub(super) fn is_retryable_stream_error(error: &str) -> bool {
    let lowered = error.to_ascii_lowercase();
    is_malformed_stream_json_error(&lowered)
        || lowered.contains("server overloaded")
        || lowered.contains("service unavailable")
        || lowered.contains("internal server error")
        || lowered.contains("invalid status code 500")
        || lowered.contains("invalid status code 502")
        || lowered.contains("invalid status code 503")
        || lowered.contains("invalid status code 504")
        || lowered.contains("http 500")
        || lowered.contains("http 502")
        || lowered.contains("http 503")
        || lowered.contains("http 504")
}

pub(super) fn is_malformed_stream_json_error(error: &str) -> bool {
    let lowered = error.to_ascii_lowercase();
    lowered.contains("jsonerror")
        || lowered.contains("eof while parsing")
        || lowered.contains("failed to parse")
}

pub(super) fn recovery_attempt_limit_for_error(error: Option<&str>) -> usize {
    if error.map(is_malformed_stream_json_error).unwrap_or(false) {
        MAX_MALFORMED_JSON_RECOVERY_ATTEMPTS
    } else {
        MAX_STREAM_ERROR_RECOVERY_ATTEMPTS
    }
}

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

pub(super) fn parse_exit_code(output: &str) -> Option<i32> {
    parse_json_number_field(output, "exit_code").and_then(|value| i32::try_from(value).ok())
}
