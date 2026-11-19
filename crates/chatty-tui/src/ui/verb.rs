//! Tool-row wording for the terminal: the verbs, subjects, activity classes
//! and diff counts come from `chatty_core::presentation::tool_verb`, shared
//! with the desktop transcript (AGE-1318). What stays here is the terminal's
//! tally sentence and change tray.

use chatty_core::models::message_types::ToolCallState as CoreToolCallState;
use chatty_core::presentation::tool_verb::{
    ToolKind, classify_tool, diff_stats, known_verb, subject_for,
};

use crate::engine::{ToolCallInfo, ToolCallState};

/// `Read` / `Reading` / `Failed Read` for a tool the table knows; `None` for
/// anything else, which keeps its raw `name(args)` header.
pub fn known_row_verb(tool_name: &str, state: &ToolCallState) -> Option<String> {
    known_verb(tool_name, &core_state(state))
}

fn core_state(state: &ToolCallState) -> CoreToolCallState {
    match state {
        ToolCallState::Running => CoreToolCallState::Running,
        ToolCallState::Success => CoreToolCallState::Success,
        ToolCallState::Error => CoreToolCallState::Error(String::new()),
    }
}

/// `(added, removed)` for an edit tool; an edit with nothing to count still
/// reads `+0 −0` here, so it shows as a change. `None` for tools that do not
/// edit.
pub fn edit_stats(tool_name: &str, input: &str, output: Option<&str>) -> Option<(usize, usize)> {
    diff_stats(tool_name, input, output)
        .or_else(|| (classify_tool(tool_name) == ToolKind::Edit).then_some((0, 0)))
}

/// The counted sentence for a folded run of tools.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivityTally {
    pub edits: usize,
    pub explore: usize,
    pub searches: usize,
    pub external: usize,
    pub commands: usize,
    pub handoffs: usize,
}

impl ActivityTally {
    pub fn from_tools(tools: &[ToolCallInfo]) -> Self {
        let mut tally = Self::default();
        for tool in tools {
            match classify_tool(&tool.name) {
                ToolKind::Edit => tally.edits += 1,
                ToolKind::Explore => tally.explore += 1,
                ToolKind::Search => tally.searches += 1,
                ToolKind::External => tally.external += 1,
                ToolKind::Command => tally.commands += 1,
                ToolKind::Handoff => tally.handoffs += 1,
            }
        }
        tally
    }

    /// `Edited 4 files, explored 6 files, 2 searches, 1 tool, ran 1 command`.
    pub fn sentence(&self) -> String {
        let mut parts = Vec::new();
        push_count(&mut parts, "Edited ", self.edits, "file", "files");
        push_count(&mut parts, "explored ", self.explore, "file", "files");
        push_count(&mut parts, "", self.searches, "search", "searches");
        push_count(&mut parts, "", self.external, "tool", "tools");
        push_count(&mut parts, "ran ", self.commands, "command", "commands");
        push_count(
            &mut parts,
            "",
            self.handoffs,
            "browser handoff",
            "browser handoffs",
        );
        if parts.is_empty() {
            return "Worked".to_string();
        }
        parts.join(", ")
    }
}

fn push_count(parts: &mut Vec<String>, verb: &str, n: usize, one: &str, many: &str) {
    match n {
        0 => {}
        1 => parts.push(format!("{verb}1 {one}")),
        n => parts.push(format!("{verb}{n} {many}")),
    }
}

/// `4 files +58 −4` over the settled edits, or `None` when nothing was
/// edited. Files are counted by distinct subject; lines are summed.
pub fn change_tray_label<'a>(tools: impl Iterator<Item = &'a ToolCallInfo>) -> Option<String> {
    let mut files = std::collections::BTreeSet::new();
    let (mut added, mut removed) = (0, 0);
    for tc in tools.filter(|tc| matches!(tc.state, ToolCallState::Success)) {
        let Some((a, r)) = edit_stats(&tc.name, &tc.input, tc.output.as_deref()) else {
            continue;
        };
        files.insert(subject_for(&tc.name, &tc.input, ""));
        added += a;
        removed += r;
    }
    if files.is_empty() {
        return None;
    }
    let n = files.len();
    let noun = if n == 1 { "file" } else { "files" };
    Some(format!("{n} {noun} +{added} −{removed}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatty_core::models::message_types::ToolSource;

    fn tool(name: &str) -> ToolCallInfo {
        ToolCallInfo {
            id: name.to_string(),
            name: name.to_string(),
            input: String::new(),
            output: None,
            state: ToolCallState::Success,
            source: ToolSource::Local,
            execution_engine: None,
        }
    }

    /// The terminal's states map onto the shared verb table's tenses.
    #[test]
    fn states_map_to_the_shared_tenses() {
        assert_eq!(
            known_row_verb("read_file", &ToolCallState::Running).as_deref(),
            Some("Reading")
        );
        assert_eq!(
            known_row_verb("apply_diff", &ToolCallState::Error).as_deref(),
            Some("Failed Edited")
        );
        assert_eq!(known_row_verb("run_shell", &ToolCallState::Success), None);
    }

    /// Same order as the desktop's `RunTally::phrase_spans`.
    #[test]
    fn tally_sentence_matches_the_desktop_order() {
        let tally = ActivityTally {
            edits: 4,
            explore: 6,
            searches: 2,
            external: 1,
            commands: 1,
            handoffs: 0,
        };
        assert_eq!(
            tally.sentence(),
            "Edited 4 files, explored 6 files, 2 searches, 1 tool, ran 1 command"
        );
    }

    #[test]
    fn tally_counts_by_tool_class() {
        let tools = vec![tool("read_file"), tool("read_file"), tool("shell_execute")];
        assert_eq!(
            ActivityTally::from_tools(&tools).sentence(),
            "explored 2 files, ran 1 command"
        );
    }

    #[test]
    fn change_tray_counts_distinct_files_and_sums_lines() {
        let mut a = tool("write_file");
        a.input = r#"{"path":"src/a.rs","content":"x\ny"}"#.into();
        let mut b = tool("apply_diff");
        b.input = r#"{"path":"src/b.rs"}"#.into();
        b.output = Some(r#"{"insertions":5,"deletions":4}"#.into());
        let mut b_again = b.clone();
        b_again.output = Some(r#"{"insertions":1,"deletions":0}"#.into());
        let read = tool("read_file");
        let tools = [a, b, b_again, read];
        assert_eq!(
            change_tray_label(tools.iter()).as_deref(),
            Some("2 files +8 −4")
        );
        assert_eq!(change_tray_label([tool("read_file")].iter()), None);
    }

    #[test]
    fn edit_stats_only_for_edit_tools_and_zero_when_uncounted() {
        assert_eq!(edit_stats("read_file", "{}", Some("+a\n-b")), None);
        assert_eq!(edit_stats("apply_diff", "{}", Some("ok")), Some((0, 0)));
        assert_eq!(
            edit_stats("write_file", r#"{"content":"a\nb\nc"}"#, Some("ok")),
            Some((3, 0))
        );
    }
}
