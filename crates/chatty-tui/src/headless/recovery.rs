//! Stream-error and tool-result recovery heuristics.
//!
//! Pure functions that classify provider errors and tool-call outputs into
//! "retry / give up / parse exit code" decisions used by the headless
//! recovery loop in `mod.rs`. Stream-error classification is a thin adapter
//! over the shared policy in `chatty_core::services::decide_recovery`
//! (AGE-244) rather than text-sniffing the error message, since the message
//! text is no longer the source of truth — the error already carries its
//! classified `StreamErrorKind` from where it was produced.

use chatty_core::services::{
    HEADLESS_MALFORMED_JSON_RETRY_ATTEMPTS, HEADLESS_TRANSPORT_RETRY_ATTEMPTS, RecoveryAction,
    StreamError, StreamErrorKind, StreamSurface, decide_recovery,
};

use super::*;
use crate::engine::{ToolCallInfo, ToolCallState};

/// Whether the shared recovery policy ever retries this error's kind on the
/// headless surface. The caller tracks its own attempt count separately and
/// compares it against `recovery_attempt_limit_for_error`.
pub(super) fn is_retryable_stream_error(error: &StreamError) -> bool {
    !matches!(
        decide_recovery(error.kind, StreamSurface::Headless, 0),
        RecoveryAction::Stop
    )
}

pub(super) fn recovery_attempt_limit_for_error(error: Option<&StreamError>) -> usize {
    match error.map(|e| e.kind) {
        Some(StreamErrorKind::MalformedToolCall) => HEADLESS_MALFORMED_JSON_RETRY_ATTEMPTS,
        _ => HEADLESS_TRANSPORT_RETRY_ATTEMPTS,
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
