use chatty_core::models::message_types::ToolCallState;
use chatty_core::presentation::tool_verb::{
    diff_stats, subject_for, terminal_read_subject, verb_for,
};

/// Parts of a tool row label. Verb tense encodes state; subject is the path/query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolRowLabel {
    pub verb: String,
    pub subject: String,
    pub added: Option<usize>,
    pub removed: Option<usize>,
}

impl ToolRowLabel {
    pub fn headline(&self) -> String {
        if self.subject.is_empty() {
            self.verb.clone()
        } else {
            format!("{} {}", self.verb, self.subject)
        }
    }
}

/// Label for a tool row. Prefer path from input over the generic "file"/"directory"
/// suffix in `friendly_tool_name`.
pub(crate) fn tool_row_label(
    display_name: &str,
    tool_name: &str,
    state: &ToolCallState,
    input: &str,
    output: Option<&str>,
) -> ToolRowLabel {
    let verb = verb_for(tool_name, display_name, state);
    let subject = if tool_name == "terminal_read" {
        terminal_read_subject(output)
    } else {
        subject_for(tool_name, input, display_name)
    };
    let (added, removed) = diff_stats(tool_name, input, output).unzip();
    ToolRowLabel {
        verb,
        subject,
        added,
        removed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_shows_filename_and_past_tense() {
        let label = tool_row_label(
            "Reading file",
            "read_file",
            &ToolCallState::Success,
            r#"{"path":"docs/poem.md"}"#,
            None,
        );
        assert_eq!(label.headline(), "Read docs/poem.md");
        assert!(label.added.is_none());
    }

    /// The generic fallback turned `terminal_read` into "terminaled read".
    #[test]
    fn terminal_read_reads_as_reading_the_terminal() {
        let label = tool_row_label(
            "terminal_read",
            "terminal_read",
            &ToolCallState::Success,
            r#"{"lines":80}"#,
            None,
        );
        assert_eq!(label.headline(), "Read terminal");

        // AGE-583: once read, which terminal and how many lines were sent.
        let output = serde_json::json!({
            "terminal": "term-1",
            "title": "bash — chatty2",
            "text": "a\nb\nerror: boom",
            "cursor_row": 2, "cols": 80, "rows": 24, "truncated": false,
        })
        .to_string();
        let label = tool_row_label(
            "terminal_read",
            "terminal_read",
            &ToolCallState::Success,
            "{}",
            Some(&output),
        );
        assert_eq!(label.headline(), "Read terminal · bash — chatty2 · 3 lines");
    }

    /// AGE-584: a command run in the human's terminal says where it ran.
    #[test]
    fn terminal_run_reads_as_running_in_your_terminal() {
        let label = tool_row_label(
            "terminal_run",
            "terminal_run",
            &ToolCallState::Success,
            r#"{"terminal":"term-2","command":"npm run dev"}"#,
            None,
        );
        assert_eq!(label.headline(), "Ran in your terminal npm run dev");
    }

    #[test]
    fn browser_handoffs_read_as_past_tense_sentences_with_the_url() {
        let take = tool_row_label(
            "Taking control of the browser",
            "browser_take_control",
            &ToolCallState::Success,
            r#"{"url":"https://example.com"}"#,
            None,
        );
        assert_eq!(
            take.headline(),
            "Took control of the browser https://example.com"
        );

        let release = tool_row_label(
            "Handing the browser back",
            "browser_release_control",
            &ToolCallState::Success,
            r#"{"url":"https://example.com/docs"}"#,
            None,
        );
        assert_eq!(
            release.headline(),
            "Handed the browser back https://example.com/docs"
        );
    }

    #[test]
    fn write_shows_path_and_line_counts() {
        let label = tool_row_label(
            "Writing file",
            "write_file",
            &ToolCallState::Success,
            "{\"path\":\"notes.md\",\"content\":\"line1\\nline2\\nline3\\n\"}",
            Some(r#"{"path":"notes.md","overwritten":false,"bytes_written":18}"#),
        );
        assert_eq!(label.headline(), "Wrote notes.md");
        assert_eq!(label.added, Some(3));
        assert_eq!(label.removed, Some(0));
    }
}
