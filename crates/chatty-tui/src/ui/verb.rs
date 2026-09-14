//! Tool-row wording shared in *semantics* with the desktop transcript
//! (`chatty-gpui/src/chatty/views/transcript/{verb,activity}.rs`): the same
//! tense verbs per tool, the same activity classes, the same tally order
//! (edits → explored → searches → tools → commands → handoffs). The widgets
//! are not shared — this is the terminal's copy of the tables (AGE-136).

use std::path::Path;

use chatty_core::services::is_agent_todo_tool;

use crate::engine::{ToolCallInfo, ToolCallState};

/// `Read` / `Reading` / `Failed Read` for a tool the table knows; `None` for
/// anything else, which keeps its raw `name(args)` header.
pub fn verb_for(tool_name: &str, state: &ToolCallState) -> Option<String> {
    let (running, done) = match tool_name {
        "read_file" | "read_binary" | "read_excel" | "read_skill" => ("Reading", "Read"),
        "list_directory" | "list_agents" | "list_mcp_services" | "list_tools" => {
            ("Listing", "Listed")
        }
        "write_file" | "write_excel" | "write_docx" | "write_pptx" => ("Writing", "Wrote"),
        "create_directory" => ("Creating", "Created"),
        "delete_file" => ("Deleting", "Deleted"),
        "move_file" => ("Moving", "Moved"),
        "apply_diff" => ("Editing", "Edited"),
        "glob_search" | "search_code" | "search_web" | "search_memory" | "find_files" => {
            ("Searching", "Searched")
        }
        "shell_execute" | "execute_code" | "daytona_run" => ("Running", "Ran"),
        "compile_typst" => ("Generating", "Generated"),
        "file_structure_detector" => ("Mapping", "Mapped"),
        "fetch" => ("Fetching", "Fetched"),
        "git_diff" => ("Diffing", "Diffed"),
        "git_status" => ("Checking", "Checked"),
        "browser_take_control" => (
            "Taking control of the browser",
            "Took control of the browser",
        ),
        "browser_release_control" => ("Handing the browser back", "Handed the browser back"),
        _ => return None,
    };
    Some(match state {
        ToolCallState::Running => running.to_string(),
        ToolCallState::Success => done.to_string(),
        ToolCallState::Error => format!("Failed {done}"),
    })
}

/// The path, query or command a tool call is about, from its JSON input.
pub fn subject_for(input: &str) -> String {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(input) else {
        return String::new();
    };
    for key in [
        "path",
        "file_path",
        "filename",
        "output_path",
        "directory",
        "dir",
    ] {
        if let Some(v) = json.get(key).and_then(|v| v.as_str())
            && !v.is_empty()
        {
            return short_path(Path::new(v));
        }
    }
    for key in ["query", "pattern", "command", "url", "task"] {
        if let Some(v) = json.get(key).and_then(|v| v.as_str()) {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                return truncate(trimmed, 64);
            }
        }
    }
    String::new()
}

fn short_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    if let Some(name) = path.file_name().and_then(|n| n.to_str())
        && s.len() > 48
    {
        return name.to_string();
    }
    truncate(&s, 64)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let trimmed: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{trimmed}…")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
    Edit,
    Explore,
    Search,
    External,
    Command,
    Handoff,
}

pub fn classify_tool(name: &str) -> ToolKind {
    let n = name.to_ascii_lowercase();
    if n == "browser_take_control" || n == "browser_release_control" {
        return ToolKind::Handoff;
    }
    if is_agent_todo_tool(&n) {
        return ToolKind::Explore;
    }
    if n.contains("diff") || n.contains("edit") || n.contains("write") || n.contains("apply") {
        ToolKind::Edit
    } else if n.contains("search") || n.contains("grep") || n.contains("glob") {
        ToolKind::Search
    } else if n.contains("web") || n.contains("fetch") || n.contains("http") || n.contains("mcp") {
        ToolKind::External
    } else if n.contains("bash")
        || n.contains("shell")
        || n.contains("exec")
        || n.contains("command")
    {
        ToolKind::Command
    } else {
        ToolKind::Explore
    }
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

/// `(added, removed)` for an edit tool, from a structured result, a unified
/// diff in the output, or the new content in the input. `None` for tools
/// that do not edit.
pub fn diff_stats(tool_name: &str, input: &str, output: Option<&str>) -> Option<(usize, usize)> {
    if classify_tool(tool_name) != ToolKind::Edit {
        return None;
    }
    if let Some(out) = output {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(out) {
            let insertions = json
                .get("insertions")
                .or_else(|| json.get("added"))
                .and_then(|v| v.as_u64());
            let deletions = json
                .get("deletions")
                .or_else(|| json.get("removed"))
                .and_then(|v| v.as_u64());
            if insertions.is_some() || deletions.is_some() {
                return Some((
                    insertions.unwrap_or(0) as usize,
                    deletions.unwrap_or(0) as usize,
                ));
            }
        }
        let (a, r) = count_diff_lines(out);
        if a > 0 || r > 0 {
            return Some((a, r));
        }
    }
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(input)
        && let Some(content) = json.get("content").and_then(|v| v.as_str())
        && !content.is_empty()
    {
        return Some((content.lines().count().max(1), 0));
    }
    Some((0, 0))
}

/// `4 files +58 −4` over the settled edits, or `None` when nothing was
/// edited. Files are counted by distinct subject; lines are summed.
pub fn change_tray_label<'a>(tools: impl Iterator<Item = &'a ToolCallInfo>) -> Option<String> {
    let mut files = std::collections::BTreeSet::new();
    let (mut added, mut removed) = (0, 0);
    for tc in tools.filter(|tc| matches!(tc.state, ToolCallState::Success)) {
        let Some((a, r)) = diff_stats(&tc.name, &tc.input, tc.output.as_deref()) else {
            continue;
        };
        files.insert(subject_for(&tc.input));
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

fn count_diff_lines(output: &str) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for line in output.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
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

    #[test]
    fn verbs_change_tense_with_state() {
        assert_eq!(
            verb_for("read_file", &ToolCallState::Running).as_deref(),
            Some("Reading")
        );
        assert_eq!(
            verb_for("read_file", &ToolCallState::Success).as_deref(),
            Some("Read")
        );
        assert_eq!(
            verb_for("apply_diff", &ToolCallState::Error).as_deref(),
            Some("Failed Edited")
        );
        assert_eq!(verb_for("run_shell", &ToolCallState::Success), None);
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
    fn diff_stats_only_for_edit_tools() {
        assert_eq!(diff_stats("read_file", "{}", Some("+a\n-b")), None);
        assert_eq!(
            diff_stats(
                "apply_diff",
                "{}",
                Some(r#"{"insertions":3,"deletions":1}"#)
            ),
            Some((3, 1))
        );
        assert_eq!(
            diff_stats("write_file", r#"{"content":"a\nb\nc"}"#, Some("ok")),
            Some((3, 0))
        );
    }
}
