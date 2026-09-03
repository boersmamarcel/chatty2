//! Locate the parent assistant bubble when a sub-agent progress row is last.
//!
//! After a sub-agent starts, follow-up tokens belong in a **continuation**
//! bubble *after* the progress row — not in the pre-tool parent above it.

use super::super::message_component::{DisplayMessage, MessageRole};

/// Index of the streaming assistant that should receive parent-stream
/// updates (text, tools, finalize).
///
/// When `skip` is the sub-agent progress row, only a bubble *after* that
/// row counts (the continuation). Pre-tool text stays above the trace.
pub(super) fn index_of_parent_streaming_assistant(
    messages: &[DisplayMessage],
    skip: Option<usize>,
) -> Option<usize> {
    messages.iter().enumerate().rev().find_map(|(i, m)| {
        if skip.is_some_and(|s| i <= s) {
            return None;
        }
        (matches!(m.role, MessageRole::Assistant) && m.is_streaming).then_some(i)
    })
}

/// Last assistant bubble that is not the sub-agent progress row.
/// Prefers the continuation below the trace when it exists.
pub(super) fn index_of_parent_assistant(
    messages: &[DisplayMessage],
    skip: Option<usize>,
) -> Option<usize> {
    messages.iter().enumerate().rev().find_map(|(i, m)| {
        if Some(i) == skip {
            return None;
        }
        matches!(m.role, MessageRole::Assistant).then_some(i)
    })
}

/// Clear `is_streaming` on every message, returning how many were still set.
///
/// The teardown paths (`finalize_assistant_message`, `mark_message_cancelled`)
/// each clear exactly one message — the parent bubble. Any other row left
/// streaming outlived its stream and kept the running footer alive with no
/// turn header above it (AGE-189). Nothing is streaming once the stream has
/// ended, so the sweep is unconditional.
pub(super) fn clear_streaming_flags(messages: &mut [DisplayMessage]) -> usize {
    let mut cleared = 0;
    for msg in messages.iter_mut() {
        if msg.is_streaming {
            msg.is_streaming = false;
            cleared += 1;
        }
    }
    cleared
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(content: &str, is_streaming: bool) -> DisplayMessage {
        DisplayMessage {
            role: MessageRole::Assistant,
            content: content.to_string(),
            is_streaming,
            system_trace_view: None,
            live_trace: None,
            is_markdown: true,
            attachments: Vec::new(),
            feedback: None,
            history_index: None,
        }
    }

    #[test]
    fn continuation_after_progress_is_preferred() {
        let messages = vec![
            assistant("pre-tool", false),
            assistant("", false), // finalized sub-agent progress row
            assistant("follow-up quote", true),
        ];

        assert_eq!(
            index_of_parent_streaming_assistant(&messages, Some(1)),
            Some(2)
        );
        assert_eq!(index_of_parent_assistant(&messages, Some(1)), Some(2));
    }

    #[test]
    fn no_continuation_does_not_fall_back_above_progress() {
        let messages = vec![
            assistant("pre-tool", true),
            assistant("", true), // in-flight progress
        ];

        assert_eq!(
            index_of_parent_streaming_assistant(&messages, Some(1)),
            None
        );
        assert_eq!(index_of_parent_assistant(&messages, Some(1)), Some(0));
    }

    #[test]
    fn parent_streaming_is_last_when_no_progress_row() {
        let messages = vec![assistant("hello", true)];

        assert_eq!(
            index_of_parent_streaming_assistant(&messages, None),
            Some(0)
        );
        assert_eq!(index_of_parent_assistant(&messages, None), Some(0));
    }

    // -------------------------------------------------------------------
    // Streaming-flag sweep (AGE-188 / AGE-189)
    // -------------------------------------------------------------------

    /// The shape that kept the footer running with no turn header above it: a
    /// sub-agent progress row whose own finalize never arrived, so the
    /// parent-only teardown left it streaming forever.
    #[test]
    fn sweep_clears_an_orphaned_progress_row() {
        let mut messages = vec![
            assistant("answer", false),
            assistant("", true), // orphaned sub-agent progress row
            assistant("continuation", true),
        ];

        let cleared = clear_streaming_flags(&mut messages);

        assert_eq!(cleared, 2, "both streaming rows must be cleared");
        assert!(
            messages.iter().all(|m| !m.is_streaming),
            "nothing may still be streaming after the stream ended"
        );
    }

    #[test]
    fn sweep_is_a_no_op_when_nothing_is_streaming() {
        let mut messages = vec![assistant("a", false), assistant("b", false)];
        assert_eq!(clear_streaming_flags(&mut messages), 0);
    }

    #[test]
    fn sweep_is_idempotent() {
        let mut messages = vec![assistant("a", true)];
        assert_eq!(clear_streaming_flags(&mut messages), 1);
        assert_eq!(clear_streaming_flags(&mut messages), 0);
    }
}
