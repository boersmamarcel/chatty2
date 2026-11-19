//! Tool-call wording shared by the desktop transcript and the terminal UI:
//! what class of work a tool is, the tense verb and subject of its row, and
//! the added/removed line counts of an edit (AGE-1318). The frontends only
//! style the result.

use std::path::{Path, PathBuf};

use crate::models::message_types::ToolCallState;
use crate::services::is_agent_todo_tool;
use crate::tools::plugin_tool::plugin_tool_display_name;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
    Edit,
    Explore,
    Search,
    External,
    Command,
    /// A browser control handoff between the user and the agent (AGE-156):
    /// not something the agent did, so it must not be counted as one of
    /// its edits, searches or explorations.
    Handoff,
}

pub fn classify_tool(name: &str) -> ToolKind {
    let n = name.to_ascii_lowercase();
    if n == "browser_take_control" || n == "browser_release_control" {
        return ToolKind::Handoff;
    }
    if n == "terminal_read" {
        // A read of the user's terminal, not a file explored or a command run.
        return ToolKind::External;
    }
    if n == "terminal_run" {
        // A command run in the user's terminal (AGE-584).
        return ToolKind::Command;
    }
    if plugin_tool_display_name(name).is_some() {
        // A spec's plugin tool (PL-U2) counts as a tool, whatever its name.
        return ToolKind::External;
    }
    if is_agent_todo_tool(&n) {
        // Agent plan tools are Plan blocks, not edits.
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

/// `Read` / `Reading` / `Failed Read` for a tool the table knows; `None` for
/// anything else. The terminal keeps its raw `name(args)` header for those;
/// [`verb_for`] words them instead.
pub fn known_verb(tool_name: &str, state: &ToolCallState) -> Option<String> {
    // A spec's plugin tool (PL-U2): one generic verb, whatever the plugin.
    if let Some(plugin_tool) = plugin_tool_display_name(tool_name) {
        return Some(match state {
            ToolCallState::Running => format!("Running {plugin_tool}"),
            ToolCallState::Success => format!("Ran {plugin_tool}"),
            ToolCallState::Error(_) => format!("Failed to run {plugin_tool}"),
        });
    }
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
        "terminal_read" => ("Reading terminal", "Read terminal"),
        "terminal_run" => ("Running in your terminal", "Ran in your terminal"),
        // Synthetic rows for the human-takeover handoffs (AGE-156).
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
        ToolCallState::Error(_) => format!("Failed {done}"),
    })
}

/// The row's verb: the table's, or one made from the display name (or the
/// tool identifier) for a tool the table does not know.
pub fn verb_for(tool_name: &str, display_name: &str, state: &ToolCallState) -> String {
    known_verb(tool_name, state).unwrap_or_else(|| tense_unknown(display_name, tool_name, state))
}

/// "· bash — chatty2 · 42 lines" for a `terminal_read` that read a terminal
/// (AGE-583): which terminal, and how much of it was sent to the model.
/// Empty while it runs, for a list, and on failure.
pub fn terminal_read_subject(output: Option<&str>) -> String {
    let Some(json) = output.and_then(|out| serde_json::from_str::<serde_json::Value>(out).ok())
    else {
        return String::new();
    };
    let Some(text) = json.get("text").and_then(|v| v.as_str()) else {
        return String::new();
    };
    let lines = text.lines().count();
    let count = format!("{lines} line{}", if lines == 1 { "" } else { "s" });
    match json.get("title").and_then(|v| v.as_str()) {
        Some(title) if !title.trim().is_empty() => format!("· {} · {count}", truncate(title, 48)),
        _ => format!("· {count}"),
    }
}

fn tense_unknown(display_name: &str, tool_name: &str, state: &ToolCallState) -> String {
    let phrase = {
        let display = display_name.trim();
        if !display.is_empty() && !display.contains('_') {
            strip_generic_object(display)
        } else {
            identifier_phrase(tool_name)
        }
    };
    match state {
        ToolCallState::Running => format!("{phrase}…"),
        ToolCallState::Success => past_phrase(&phrase),
        ToolCallState::Error(_) => format!("Failed {}", past_phrase(&phrase)),
    }
}

fn strip_generic_object(phrase: &str) -> String {
    // "Reading file" → "Reading"; path is attached separately.
    let lower = phrase.to_ascii_lowercase();
    for suffix in [
        " file",
        " directory",
        " spreadsheet",
        " page",
        " binary file",
    ] {
        if lower.ends_with(suffix) {
            return phrase[..phrase.len() - suffix.len()].to_string();
        }
    }
    phrase.to_string()
}

/// The path, query or command a tool call is about.
pub fn subject_for(tool_name: &str, input: &str, display_name: &str) -> String {
    if let Some(path) = tool_file_path(input) {
        return short_path(&path);
    }
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(input) {
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
    }
    // The handoff rows' only subject is the URL, returned above when present.
    // With none (the session was still opening), the display-name fallback
    // below would split "Taking control of the browser" and read
    // "Took control of the browser control of the browser".
    if matches!(
        tool_name,
        "browser_take_control" | "browser_release_control"
    ) {
        return String::new();
    }
    // If display already embeds a specific subject ("Read README.md"), keep it
    // only when it isn't the generic friendly name.
    let display = display_name.trim();
    if !display.is_empty()
        && !display.contains('_')
        && !is_generic_friendly(display)
        && display.split_whitespace().count() > 1
    {
        let mut parts = display.splitn(2, char::is_whitespace);
        let _verb = parts.next();
        if let Some(rest) = parts.next() {
            let rest = rest.trim();
            if !rest.is_empty()
                && !matches!(
                    rest.to_ascii_lowercase().as_str(),
                    "file" | "directory" | "spreadsheet" | "page" | "files" | "code"
                )
            {
                return rest.to_string();
            }
        }
    }
    let _ = tool_name;
    String::new()
}

fn is_generic_friendly(display: &str) -> bool {
    let lower = display.to_ascii_lowercase();
    lower.ends_with(" file")
        || lower.ends_with(" directory")
        || lower.ends_with(" spreadsheet")
        || lower.ends_with(" page")
        || lower.ends_with(" files")
        || lower.ends_with(" code")
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

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let trimmed: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{trimmed}…")
    }
}

/// `(added, removed)` of an edit tool, from a structured result, a unified
/// diff in the output, or the content in the input; `None` for a tool that
/// does not edit or a call with nothing to count.
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

    if let Ok(json) = serde_json::from_str::<serde_json::Value>(input) {
        if let Some(content) = json.get("content").and_then(|v| v.as_str()) {
            let lines = content
                .lines()
                .count()
                .max(if content.is_empty() { 0 } else { 1 });
            if lines > 0 {
                return Some((lines, 0));
            }
        }
        let old = json
            .get("old_content")
            .or_else(|| json.get("old_string"))
            .or_else(|| json.get("old"))
            .and_then(|v| v.as_str());
        let new = json
            .get("new_content")
            .or_else(|| json.get("new_string"))
            .or_else(|| json.get("new"))
            .and_then(|v| v.as_str());
        if let (Some(old), Some(new)) = (old, new) {
            let removed = old.lines().count();
            let added = new.lines().count();
            if added > 0 || removed > 0 {
                return Some((added, removed));
            }
        }
    }

    None
}

pub fn count_diff_lines(output: &str) -> (usize, usize) {
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

/// Turn `mcp:server.search_files` / `search_files` into `search files`.
fn identifier_phrase(tool_name: &str) -> String {
    let ident = tool_name.rsplit([':', '/']).next().unwrap_or(tool_name);
    let ident = if ident.contains(' ') {
        ident
    } else {
        ident.rsplit('.').next().unwrap_or(ident)
    };
    ident.replace('_', " ")
}

fn past_phrase(phrase: &str) -> String {
    let mut parts = phrase.splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or(phrase);
    let rest = parts.next();
    let past = ensure_past(first);
    match rest {
        Some(rest) if !rest.is_empty() => format!("{past} {rest}"),
        _ => past,
    }
}

fn ensure_past(word: &str) -> String {
    let lower = word.to_ascii_lowercase();
    if let Some(past) = gerund_past(&lower) {
        return match_case(word, past);
    }
    if lower.ends_with("ed") || irregular_past(&lower).is_some_and(|p| p == lower) {
        return word.to_string();
    }
    if let Some(past) = irregular_past(&lower) {
        return match_case(word, past);
    }
    past_tense(word)
}

/// "Reading" / "writing" / "listing" → past without "Readinged".
fn gerund_past(lower: &str) -> Option<&'static str> {
    Some(match lower {
        "reading" => "read",
        "writing" => "wrote",
        "running" => "ran",
        "listing" => "listed",
        "creating" => "created",
        "deleting" => "deleted",
        "moving" => "moved",
        "searching" => "searched",
        "finding" => "found",
        "fetching" => "fetched",
        "checking" => "checked",
        "viewing" => "viewed",
        "staging" => "staged",
        "committing" => "committed",
        "switching" => "switched",
        "attaching" => "attached",
        "generating" => "generated",
        "inspecting" => "inspected",
        "extracting" => "extracted",
        "rendering" => "rendered",
        "querying" => "queried",
        "executing" => "executed",
        "saving" => "saved",
        "loading" => "loaded",
        "calling" => "called",
        "delegating" => "delegated",
        "browsing" => "browsed",
        "publishing" => "published",
        "applying" => "applied",
        "editing" => "edited",
        "looking" => "looked",
        "changing" => "changed",
        "setting" => "set",
        "mapping" => "mapped",
        "diffing" => "diffed",
        _ => return None,
    })
}

fn irregular_past(lower: &str) -> Option<&'static str> {
    Some(match lower {
        "read" => "read",
        "run" | "ran" => "ran",
        "write" | "wrote" => "wrote",
        "make" | "made" => "made",
        "find" | "found" => "found",
        "get" | "got" => "got",
        "send" | "sent" => "sent",
        "build" | "built" => "built",
        "set" => "set",
        _ => return None,
    })
}

fn match_case(original: &str, past: &str) -> String {
    if original.chars().next().is_some_and(|c| c.is_uppercase()) {
        let mut chars = past.chars();
        match chars.next() {
            Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
            None => past.to_string(),
        }
    } else {
        past.to_string()
    }
}

fn past_tense(verb: &str) -> String {
    if verb.ends_with('e') {
        format!("{verb}d")
    } else if verb.ends_with('y')
        && verb.len() > 1
        && verb
            .chars()
            .nth_back(1)
            .is_some_and(|c| !"aeiou".contains(c))
    {
        format!("{}ied", &verb[..verb.len() - 1])
    } else {
        format!("{verb}ed")
    }
}

/// Pull a file path out of a tool's JSON-ish input or output.
///
/// Keys checked (longest first so `output_path` wins over the `path` substring):
/// `output_path`, `saved_path`, `file_path`, `filename`, `path`.
pub fn tool_file_path(input: &str) -> Option<PathBuf> {
    // Prefer structured JSON when present.
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(input) {
        for key in [
            "output_path",
            "saved_path",
            "save_path",
            "file_path",
            "filename",
            "path",
        ] {
            if let Some(v) = json.get(key).and_then(|v| v.as_str()) {
                let trimmed = v.trim();
                if !trimmed.is_empty() {
                    return Some(PathBuf::from(trimmed));
                }
            }
        }
    }
    // Fallback: substring scan (longest keys first).
    for key in [
        "output_path",
        "saved_path",
        "save_path",
        "file_path",
        "filename",
        "path",
    ] {
        if let Some(idx) = input.find(key)
            && let Some(rest) = input.get(idx + key.len()..)
        {
            let trimmed = rest.trim_start_matches([' ', ':', '=', '"', '\'']);
            let end = trimmed
                .find(|c: char| c == '"' || c == '\'' || c.is_whitespace() || c == ',')
                .unwrap_or(trimmed.len());
            let path = &trimmed[..end];
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verb(tool: &str, state: ToolCallState) -> String {
        verb_for(tool, tool, &state)
    }

    #[test]
    fn classify_tool_sorts_tools_into_activity_classes() {
        assert_eq!(classify_tool("read_file"), ToolKind::Explore);
        assert_eq!(classify_tool("search_code"), ToolKind::Search);
        assert_eq!(classify_tool("apply_diff"), ToolKind::Edit);
        assert_eq!(classify_tool("shell_execute"), ToolKind::Command);
        assert_eq!(classify_tool("fetch"), ToolKind::External);
        assert_eq!(classify_tool("write_todos"), ToolKind::Explore);
        assert_eq!(classify_tool("verify_completion"), ToolKind::Explore);
    }

    /// AGE-584 / AGE-1318: the terminal's `terminal_read` is a tool call and
    /// `terminal_run` a command in both frontends (the TUI used to count them
    /// as explored files by the generic fallback).
    #[test]
    fn terminal_tools_classify_the_same_everywhere() {
        assert_eq!(classify_tool("terminal_read"), ToolKind::External);
        assert_eq!(classify_tool("terminal_run"), ToolKind::Command);
    }

    #[test]
    fn browser_handoffs_are_not_agent_work() {
        assert_eq!(classify_tool("browser_take_control"), ToolKind::Handoff);
        assert_eq!(classify_tool("browser_release_control"), ToolKind::Handoff);
        assert_eq!(classify_tool("browser_navigate"), ToolKind::Explore);
    }

    /// PL-U2: a spec's plugin tool reads as running `<plugin>.<tool>`,
    /// whichever plugin, and counts as a tool.
    #[test]
    fn plugin_tools_read_as_ran_plugin_dot_tool() {
        assert_eq!(
            verb("echo-agent__reverse", ToolCallState::Running),
            "Running echo-agent.reverse"
        );
        assert_eq!(
            verb("echo-agent__reverse", ToolCallState::Success),
            "Ran echo-agent.reverse"
        );
        assert_eq!(
            verb(
                "spin__spin",
                ToolCallState::Error("deadline exceeded".into())
            ),
            "Failed to run spin.spin"
        );
        assert_eq!(classify_tool("spin__spin"), ToolKind::External);
        // A plugin tool is never an edit, whatever its name says.
        assert_eq!(classify_tool("echo__write_file"), ToolKind::External);
        assert_eq!(
            diff_stats("echo__write_file", r#"{"content":"a"}"#, None),
            None
        );
    }

    #[test]
    fn verbs_change_tense_with_state() {
        assert_eq!(verb("read_file", ToolCallState::Running), "Reading");
        assert_eq!(verb("read_file", ToolCallState::Success), "Read");
        assert_eq!(
            verb("apply_diff", ToolCallState::Error(String::new())),
            "Failed Edited"
        );
    }

    /// The terminal keeps the raw `name(args)` header for an unknown tool.
    #[test]
    fn known_verb_is_none_for_a_tool_the_table_does_not_know() {
        assert_eq!(known_verb("run_shell", &ToolCallState::Success), None);
        assert_eq!(
            known_verb("terminal_read", &ToolCallState::Success).as_deref(),
            Some("Read terminal")
        );
        assert_eq!(
            known_verb("terminal_run", &ToolCallState::Success).as_deref(),
            Some("Ran in your terminal")
        );
    }

    #[test]
    fn gerund_display_names_do_not_double_suffix() {
        assert_eq!(
            verb_for("some_tool", "Listing directory", &ToolCallState::Success),
            "Listed"
        );
        assert_eq!(
            verb_for("some_tool", "Reading file", &ToolCallState::Running),
            "Reading…"
        );
    }

    #[test]
    fn file_structure_detector_reads_as_mapped() {
        assert_eq!(
            verb("file_structure_detector", ToolCallState::Success),
            "Mapped"
        );
    }

    #[test]
    fn subject_is_the_path_query_or_command() {
        assert_eq!(
            subject_for("read_file", r#"{"path":"docs/poem.md"}"#, ""),
            "docs/poem.md"
        );
        assert_eq!(
            subject_for("shell_execute", r#"{"command":"  ls -la "}"#, ""),
            "ls -la"
        );
        assert_eq!(subject_for("read_file", "not json", ""), "");
        let long = format!("{}/x.rs", "d".repeat(60));
        assert_eq!(
            subject_for("read_file", &format!(r#"{{"path":"{long}"}}"#), ""),
            "x.rs"
        );
    }

    #[test]
    fn subject_falls_back_to_the_display_name() {
        assert_eq!(subject_for("read_file", "", "Read README.md"), "README.md");
        assert_eq!(subject_for("read_file", "", "Reading file"), "");
    }

    #[test]
    fn browser_handoffs_take_the_url_and_never_the_display_name() {
        assert_eq!(
            subject_for(
                "browser_take_control",
                r#"{"url":"https://example.com"}"#,
                "Taking control of the browser"
            ),
            "https://example.com"
        );
        assert_eq!(
            subject_for(
                "browser_take_control",
                r#"{"url":""}"#,
                "Taking control of the browser"
            ),
            ""
        );
    }

    #[test]
    fn terminal_read_subject_names_the_terminal_and_lines() {
        let output = serde_json::json!({
            "terminal": "term-1",
            "title": "bash — chatty2",
            "text": "a\nb\nerror: boom",
        })
        .to_string();
        assert_eq!(
            terminal_read_subject(Some(&output)),
            "· bash — chatty2 · 3 lines"
        );
        assert_eq!(terminal_read_subject(None), "");
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

    #[test]
    fn diff_stats_reads_a_unified_diff_and_old_new_inputs() {
        assert_eq!(
            diff_stats("apply_diff", "{}", Some("--- a\n+++ b\n+x\n+y\n-z")),
            Some((2, 1))
        );
        assert_eq!(
            diff_stats(
                "apply_diff",
                r#"{"old_content":"a\n","new_content":"a\nb\nc\n"}"#,
                None
            ),
            Some((3, 1))
        );
        assert_eq!(diff_stats("apply_diff", "{}", Some("ok")), None);
    }

    #[test]
    fn count_diff_lines_skips_file_headers() {
        assert_eq!(count_diff_lines("--- a\n+++ b\n+x\n-y\n-z\n c"), (1, 2));
    }

    #[test]
    fn tool_file_path_prefers_the_output_path() {
        assert_eq!(
            tool_file_path(r#"{"path":"a.md","output_path":"b.pdf"}"#),
            Some(PathBuf::from("b.pdf"))
        );
        assert_eq!(tool_file_path("{}"), None);
    }
}
