//! What a delegated worker's events look like in the parent's transcript,
//! and which binary a worker runs in.
//!
//! A worker is a `chatty-tui` process the broker spawns per task
//! (ADR-0011 C2), and the parent renders each of its tool calls as one line.
//! Both halves are here because both ends need them: the runner resolves the
//! binary, the broker's mapper renders the lines.

use std::collections::HashMap;
use std::path::PathBuf;

use tracing::warn;

use crate::session::SessionEvent;

/// Compact progress text for the parent UI from a worker's event: `name` on
/// a tool start, `✓ name` / `✗ name` on its result, and the worker's stream
/// error. `names` remembers each tool call's name until its result arrives.
///
/// The broker's worker mapper renders these same strings from these same
/// events, which is what makes a delegated turn read in the parent's
/// transcript the way ADR-0011's first kill criterion was measured against.
pub fn progress_text_for_event(
    event: &SessionEvent,
    names: &mut HashMap<String, String>,
) -> Option<String> {
    match event {
        SessionEvent::ToolCallStarted { id, name } => {
            names.insert(id.clone(), name.clone());
            Some(name.clone())
        }
        SessionEvent::ToolCallResult { id, .. } => {
            names.remove(id).map(|name| format!("\u{2713} {name}"))
        }
        SessionEvent::ToolCallError { id, .. } => {
            names.remove(id).map(|name| format!("\u{2717} {name}"))
        }
        SessionEvent::Error(error) => Some(format!("error: {}", error.message)),
        _ => None,
    }
}

/// The `chatty-tui` binary a delegated task runs in: next to the current
/// binary if it is there, otherwise whatever is on `PATH`.
///
/// Used by the broker's local runner (AGE-301), which spawns the worker.
pub fn worker_executable() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("chatty-tui")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| {
            warn!("chatty-tui not found next to current binary, falling back to PATH");
            PathBuf::from("chatty-tui")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_text_names_the_tool_and_its_outcome() {
        let mut names = HashMap::new();
        let started = SessionEvent::ToolCallStarted {
            id: "c".into(),
            name: "read_file".into(),
        };
        assert_eq!(
            progress_text_for_event(&started, &mut names).as_deref(),
            Some("read_file")
        );
        let ok = SessionEvent::ToolCallResult {
            id: "c".into(),
            result: "…".into(),
        };
        assert_eq!(
            progress_text_for_event(&ok, &mut names).as_deref(),
            Some("✓ read_file")
        );
        assert!(names.is_empty(), "the name is released with its result");
        assert_eq!(
            progress_text_for_event(&SessionEvent::TurnEnded, &mut names),
            None
        );
    }

    #[test]
    fn a_failed_tool_call_is_marked_as_such() {
        let mut names = HashMap::new();
        progress_text_for_event(
            &SessionEvent::ToolCallStarted {
                id: "c".into(),
                name: "write_file".into(),
            },
            &mut names,
        );
        let failed = SessionEvent::ToolCallError {
            id: "c".into(),
            error: "permission denied".into(),
        };
        assert_eq!(
            progress_text_for_event(&failed, &mut names).as_deref(),
            Some("✗ write_file")
        );
        assert!(names.is_empty());
    }
}
