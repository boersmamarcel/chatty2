use chatty_core::models::message_types::ToolCallState;
use std::path::Path;

use super::artifact_kind::tool_file_path;

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
    let subject = subject_for(tool_name, input, display_name);
    let (added, removed) = diff_stats(tool_name, input, output);
    ToolRowLabel {
        verb,
        subject,
        added,
        removed,
    }
}

fn verb_for(tool_name: &str, display_name: &str, state: &ToolCallState) -> String {
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
        // Synthetic rows for the human-takeover handoffs (AGE-156).
        "browser_take_control" => (
            "Taking control of the browser",
            "Took control of the browser",
        ),
        "browser_release_control" => ("Handing the browser back", "Handed the browser back"),
        other => {
            return tense_unknown(display_name, other, state);
        }
    };
    match state {
        ToolCallState::Running => running.to_string(),
        ToolCallState::Success => done.to_string(),
        ToolCallState::Error(_) => format!("Failed {done}"),
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

fn subject_for(tool_name: &str, input: &str, display_name: &str) -> String {
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

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let trimmed: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{trimmed}…")
    }
}

pub(crate) fn diff_stats(
    tool_name: &str,
    input: &str,
    output: Option<&str>,
) -> (Option<usize>, Option<usize>) {
    let name = tool_name.to_ascii_lowercase();
    let is_edit = name.contains("diff")
        || name.contains("edit")
        || name.contains("write")
        || name.contains("apply");
    if !is_edit {
        return (None, None);
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
                return (
                    Some(insertions.unwrap_or(0) as usize),
                    Some(deletions.unwrap_or(0) as usize),
                );
            }
        }
        let (a, r) = count_diff_lines(out);
        if a > 0 || r > 0 {
            return (Some(a), Some(r));
        }
    }

    if let Ok(json) = serde_json::from_str::<serde_json::Value>(input) {
        if let Some(content) = json.get("content").and_then(|v| v.as_str()) {
            let lines = content
                .lines()
                .count()
                .max(if content.is_empty() { 0 } else { 1 });
            if lines > 0 {
                return (Some(lines), Some(0));
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
                return (Some(added), Some(removed));
            }
        }
    }

    (None, None)
}

pub(crate) fn count_diff_lines(output: &str) -> (usize, usize) {
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

    #[test]
    fn gerund_display_names_do_not_double_suffix() {
        let label = tool_row_label(
            "Listing directory",
            "list_directory",
            &ToolCallState::Success,
            r#"{"path":"src"}"#,
            None,
        );
        assert_eq!(label.headline(), "Listed src");
    }

    #[test]
    fn snake_case_tool_gets_human_verb() {
        let label = tool_row_label(
            "file_structure_detector",
            "file_structure_detector",
            &ToolCallState::Success,
            r#"{"path":"."}"#,
            None,
        );
        assert_eq!(label.verb, "Mapped");
        assert_eq!(label.subject, ".");
    }

    #[test]
    fn running_read_keeps_present_participle() {
        let label = tool_row_label(
            "Reading file",
            "read_file",
            &ToolCallState::Running,
            r#"{"path":"a.rs"}"#,
            None,
        );
        assert_eq!(label.verb, "Reading");
        assert_eq!(label.subject, "a.rs");
        assert_eq!(label.headline(), "Reading a.rs");
    }

    #[test]
    fn apply_diff_uses_insertions_from_output() {
        let label = tool_row_label(
            "Applying changes",
            "apply_diff",
            &ToolCallState::Success,
            r#"{"path":"prep.yml","old_content":"a\n","new_content":"a\nb\nc\n"}"#,
            Some(r#"{"path":"prep.yml","insertions":6,"deletions":3}"#),
        );
        assert_eq!(label.headline(), "Edited prep.yml");
        assert_eq!(label.added, Some(6));
        assert_eq!(label.removed, Some(3));
    }

    #[test]
    fn display_name_with_filename_still_works() {
        let label = tool_row_label(
            "Read README.md",
            "read_file",
            &ToolCallState::Success,
            "",
            None,
        );
        // No input path — fall back to subject from display name.
        assert_eq!(label.headline(), "Read README.md");
    }
}
