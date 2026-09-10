//! Condensing tool-call payloads down to something a human reads at a glance.
//!
//! The chat view shows a tool call as one header line — `shell_execute(pwd)` —
//! plus a few lines of its result. These helpers do the condensing; rendering
//! and styling stay in [`crate::ui::chat_view`].

/// Upper bound on the argument preview, even on a very wide terminal — past
/// this the preview stops being a glance.
const INPUT_PREVIEW_MAX: usize = 56;

/// Preview floor, so a narrow terminal still shows something identifying
/// rather than collapsing to a bare `…`.
pub const INPUT_PREVIEW_MIN: usize = 12;

/// How many output lines survive folding before the `… +N lines` marker.
pub const COLLAPSED_OUTPUT_LINES: usize = 3;

/// Input keys worth showing, most identifying first. The first one present with
/// a scalar value wins.
const PRIMARY_INPUT_KEYS: &[&str] = &[
    "command",
    "cmd",
    "script",
    "file_path",
    "path",
    "url",
    "query",
    "pattern",
    "prompt",
    "title",
    "name",
    "id",
];

/// Output keys that hold the human-readable part of a structured result.
const PRIMARY_OUTPUT_KEYS: &[&str] = &[
    "message", "stdout", "output", "result", "text", "content", "summary", "error",
];

/// One-line preview of a tool call's input, for the header parentheses.
///
/// `budget` is how many columns the header has left for it. Returns an empty
/// string when there is nothing worth showing, in which case the caller
/// renders a bare `name()`.
pub fn summarize_input(input: &str, budget: usize) -> String {
    let input = input.trim();
    if input.is_empty() {
        return String::new();
    }

    let summary = match serde_json::from_str::<serde_json::Value>(input) {
        Ok(serde_json::Value::Object(map)) => summarize_object(&map),
        Ok(value) => scalar_preview(&value).unwrap_or_default(),
        Err(_) => single_line(input),
    };

    truncate(&summary, budget.clamp(INPUT_PREVIEW_MIN, INPUT_PREVIEW_MAX))
}

/// The lines of a tool result, unfolded: structured payloads give up their
/// human-readable field, everything else is used as-is.
pub fn output_lines(output: &str) -> Vec<String> {
    let output = output.trim_matches('\n');
    if output.trim().is_empty() {
        return Vec::new();
    }

    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(output)
        && let Some(text) = PRIMARY_OUTPUT_KEYS
            .iter()
            .find_map(|key| map.get(*key).and_then(|value| value.as_str()))
            .map(str::trim)
            .filter(|text| !text.is_empty())
    {
        return text.lines().map(str::to_string).collect();
    }

    output.lines().map(str::to_string).collect()
}

/// Fold `lines` to at most [`COLLAPSED_OUTPUT_LINES`], each clipped to `width`
/// columns. Returns the kept lines and how many were dropped.
///
/// Clipping to the real width matters: ratatui's `Wrap` restarts a wrapped row
/// at column 0, so an over-long result line would break the indent that makes
/// the folded block readable.
pub fn fold(lines: Vec<String>, width: usize) -> (Vec<String>, usize) {
    let hidden = lines.len().saturating_sub(COLLAPSED_OUTPUT_LINES);
    let kept = lines
        .into_iter()
        .take(COLLAPSED_OUTPUT_LINES)
        .map(|line| truncate(line.trim_end(), width.max(INPUT_PREVIEW_MIN)))
        .collect();
    (kept, hidden)
}

/// `… +3 lines` / `… +1 line`.
pub fn hidden_marker(hidden: usize) -> String {
    if hidden == 1 {
        "… +1 line".to_string()
    } else {
        format!("… +{hidden} lines")
    }
}

fn summarize_object(map: &serde_json::Map<String, serde_json::Value>) -> String {
    if let Some(preview) = PRIMARY_INPUT_KEYS
        .iter()
        .filter_map(|key| map.get(*key))
        .find_map(scalar_preview)
    {
        return preview;
    }

    // No recognized key: a compact `key=value` list. Sorted by key, because
    // `serde_json::Map` iterates in insertion order when some crate in the
    // workspace turns on `preserve_order`, and the preview should not depend
    // on which crates got built alongside this one.
    let mut pairs: Vec<String> = map
        .iter()
        .map(|(key, value)| format!("{key}={}", value_preview(value)))
        .collect();
    pairs.sort();
    pairs.join(" ")
}

/// A scalar rendered for display, or `None` for objects, arrays and nulls.
fn scalar_preview(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => {
            let text = single_line(text);
            (!text.is_empty()).then_some(text)
        }
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// Like [`scalar_preview`] but never fails — nested values collapse to a shape.
fn value_preview(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(_) => "{…}".to_string(),
        serde_json::Value::Array(items) => format!("[{}]", items.len()),
        serde_json::Value::Null => "null".to_string(),
        other => scalar_preview(other).unwrap_or_default(),
    }
}

/// Collapse newlines, tabs and runs of spaces so a value fits on one row.
fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_key_wins_over_the_rest_of_the_object() {
        assert_eq!(
            summarize_input(
                r#"{"cwd":"/tmp","command":"ls -la /notes","timeout":30}"#,
                56
            ),
            "ls -la /notes"
        );
    }

    #[test]
    fn file_and_url_shapes_use_their_own_keys() {
        assert_eq!(
            summarize_input(r#"{"file_path":"/src/main.rs","limit":40}"#, 56),
            "/src/main.rs"
        );
        assert_eq!(
            summarize_input(r#"{"url":"https://example.com/a"}"#, 56),
            "https://example.com/a"
        );
    }

    /// Sorted, so the preview does not change with `serde_json`'s
    /// `preserve_order` feature (which workspace feature unification can turn
    /// on without this crate asking for it).
    #[test]
    fn unrecognized_object_falls_back_to_sorted_key_value_pairs() {
        assert_eq!(
            summarize_input(
                r#"{"status":"done","count":3,"todos":[1,2],"meta":{"a":1}}"#,
                56
            ),
            "count=3 meta={…} status=done todos=[2]"
        );
    }

    #[test]
    fn non_json_input_is_flattened_to_one_line() {
        assert_eq!(
            summarize_input("git status\n  --short", 56),
            "git status --short"
        );
    }

    #[test]
    fn empty_input_yields_no_preview() {
        assert_eq!(summarize_input("   ", 56), "");
        assert_eq!(summarize_input("{}", 56), "");
    }

    #[test]
    fn long_preview_is_truncated_with_an_ellipsis() {
        let long = "a".repeat(200);
        let preview = summarize_input(&format!(r#"{{"command":"{long}"}}"#), 56);
        assert_eq!(preview.chars().count(), INPUT_PREVIEW_MAX);
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn structured_output_shows_its_human_field() {
        assert_eq!(
            output_lines(r#"{"message":"Todo marked done.","snapshot":{"todos":[1,2,3]}}"#),
            vec!["Todo marked done.".to_string()]
        );
    }

    #[test]
    fn structured_output_without_a_human_field_keeps_the_raw_payload() {
        assert_eq!(
            output_lines(r#"{"exit_code":0}"#),
            vec![r#"{"exit_code":0}"#.to_string()]
        );
    }

    #[test]
    fn plain_output_keeps_its_lines() {
        assert_eq!(
            output_lines("stdout line 1\nstderr line 2\n"),
            vec!["stdout line 1".to_string(), "stderr line 2".to_string()]
        );
    }

    #[test]
    fn fold_reports_the_hidden_line_count() {
        let lines: Vec<String> = (1..=9).map(|n| format!("line {n}")).collect();
        let (kept, hidden) = fold(lines, 80);
        assert_eq!(kept, vec!["line 1", "line 2", "line 3"]);
        assert_eq!(hidden, 6);
    }

    #[test]
    fn fold_hides_nothing_when_output_already_fits() {
        let (kept, hidden) = fold(vec!["only".to_string()], 80);
        assert_eq!(kept, vec!["only".to_string()]);
        assert_eq!(hidden, 0);
    }

    #[test]
    fn fold_clips_a_long_line_to_the_given_width() {
        let (kept, hidden) = fold(vec!["x".repeat(500)], 74);
        assert_eq!(kept[0].chars().count(), 74);
        assert!(kept[0].ends_with('…'));
        assert_eq!(hidden, 0);
    }

    #[test]
    fn a_narrow_terminal_still_shows_an_identifying_preview() {
        assert_eq!(
            summarize_input(r#"{"command":"cargo build --release"}"#, 0),
            "cargo build…"
        );
    }

    #[test]
    fn the_hidden_marker_is_singular_for_one_line() {
        assert_eq!(hidden_marker(1), "… +1 line");
        assert_eq!(hidden_marker(4), "… +4 lines");
    }
}
