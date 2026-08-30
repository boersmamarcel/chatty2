use chatty_core::models::message_types::ToolCallState;

/// Label for a tool row. Verb tense encodes state; the rest of the phrase is left intact.
pub(crate) fn tool_row_label(display_name: &str, tool_name: &str, state: &ToolCallState) -> String {
    let phrase = human_phrase(display_name, tool_name);
    match state {
        ToolCallState::Running => format!("{phrase}…"),
        ToolCallState::Success => past_phrase(&phrase),
        ToolCallState::Error(_) => format!("Failed {phrase}"),
    }
}

fn human_phrase(display_name: &str, tool_name: &str) -> String {
    let display = display_name.trim();
    if !display.is_empty() {
        return display.to_string();
    }
    identifier_phrase(tool_name)
}

/// Turn `mcp:server.search_files` / `search_files` into `search files`.
/// Do not split on `.` inside a human display name — that is how `README.md` became `md`.
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
    if lower.ends_with("ed") || irregular_past(&lower).is_some_and(|p| p == lower) {
        return word.to_string();
    }
    if let Some(past) = irregular_past(&lower) {
        return match_case(word, past);
    }
    past_tense(word)
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
    fn display_name_keeps_filename_and_does_not_double_suffix() {
        assert_eq!(
            tool_row_label("Searched files", "search_files", &ToolCallState::Success),
            "Searched files"
        );
        assert_eq!(
            tool_row_label("Read README.md", "read_file", &ToolCallState::Success),
            "Read README.md"
        );
        assert_eq!(
            tool_row_label("Ran git status", "bash", &ToolCallState::Success),
            "Ran git status"
        );
        assert_eq!(
            tool_row_label("Created NOTES.md", "write_file", &ToolCallState::Success),
            "Created NOTES.md"
        );
    }

    #[test]
    fn tool_name_fallback_tenses_first_word_only() {
        assert_eq!(
            tool_row_label("", "search_files", &ToolCallState::Success),
            "searched files"
        );
        assert_eq!(
            tool_row_label("", "write_file", &ToolCallState::Success),
            "wrote file"
        );
        assert_eq!(
            tool_row_label("", "mcp:fs.read_file", &ToolCallState::Running),
            "read file…"
        );
        assert_eq!(
            tool_row_label(
                "Fetched changelog",
                "web_fetch",
                &ToolCallState::Error("nope".into())
            ),
            "Failed Fetched changelog"
        );
    }
}
