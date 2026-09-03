//! Shared `@` file-mention picker helpers for the chat input, used by both
//! the GPUI desktop app and the terminal UI.
//!
//! Pure text/filesystem helpers only (AGE-172) — UI framework code
//! (rendering, focus, keyboard handling) stays in each frontend.

/// Directories and files excluded from the @ mention file list.
const AT_EXCLUDED: &[&str] = &[
    "node_modules",
    "target",
    "__pycache__",
    "dist",
    "build",
    ".git",
];

/// Maximum number of @ mention items shown in the picker.
pub const AT_MENU_MAX_ITEMS: usize = 15;

/// Read the file/directory listing for the `@` mention picker from `dir`.
/// Returns a sorted list of names, skipping hidden entries and common
/// build/dependency directories.
pub fn load_files_for_dir(dir: &std::path::Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || AT_EXCLUDED.contains(&name.as_str()) {
                return None;
            }
            Some(name)
        })
        .collect();
    files.sort_unstable();
    files
}

/// Extract the `@` query from the end of `input_text`.
///
/// Returns `Some(query)` when the text ends with `@<word>` (no whitespace
/// after `@`). Returns `None` when no `@` is present or when there is
/// whitespace after the `@`.
pub fn at_query_from(input_text: &str) -> Option<String> {
    // Work on the trailing portion of the text (handle multiline gracefully).
    let last_line = input_text.lines().next_back().unwrap_or(input_text);
    let at_pos = last_line.rfind('@')?;
    let after_at = &last_line[at_pos + 1..];
    // Close the menu as soon as the user types a space (including trailing).
    if after_at.chars().any(char::is_whitespace) {
        return None;
    }
    Some(after_at.to_ascii_lowercase())
}

/// Return the subset of `files` that match the current `@` query in
/// `input_text`, capped at [`AT_MENU_MAX_ITEMS`].
pub fn at_menu_items_for<'a>(input_text: &str, files: &'a [String]) -> Vec<&'a String> {
    let Some(query) = at_query_from(input_text) else {
        return Vec::new();
    };
    files
        .iter()
        .filter(|f| query.is_empty() || f.to_ascii_lowercase().contains(query.as_str()))
        .take(AT_MENU_MAX_ITEMS)
        .collect()
}

/// Build the replacement input text when a file is chosen from the `@`
/// mention picker. The `@<query>` suffix is replaced with `@<filename> `.
pub fn apply_at_to_input(input_text: &str, filename: &str) -> String {
    let input_text = input_text.trim_end_matches(['\r', '\n']);
    // Find the `@` that opened the menu on the last line.
    let last_line_start = input_text.rfind('\n').map(|p| p + 1).unwrap_or(0);
    let last_line = &input_text[last_line_start..];
    let at_pos_in_line = match last_line.rfind('@') {
        Some(p) => p,
        None => return format!("{} @{} ", input_text.trim_end(), filename),
    };
    let prefix_lines = &input_text[..last_line_start];
    let before_at = &last_line[..at_pos_in_line];
    if prefix_lines.is_empty() && before_at.is_empty() {
        format!("@{} ", filename)
    } else if before_at.is_empty() {
        format!("{}@{} ", prefix_lines, filename)
    } else {
        format!("{}{}@{} ", prefix_lines, before_at, filename)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_query_returns_none_when_no_at() {
        assert!(at_query_from("hello world").is_none());
        assert!(at_query_from("").is_none());
    }

    #[test]
    fn at_query_returns_query_after_at() {
        assert_eq!(at_query_from("@"), Some(String::new()));
        assert_eq!(at_query_from("@readme"), Some("readme".into()));
        assert_eq!(at_query_from("hello @src"), Some("src".into()));
    }

    #[test]
    fn at_query_closes_on_space() {
        assert!(at_query_from("@readme ").is_none());
        assert!(at_query_from("@readme.md and more").is_none());
    }

    #[test]
    fn at_menu_items_filter_by_query() {
        let files = vec![
            "README.md".to_string(),
            "src".to_string(),
            "Cargo.toml".to_string(),
        ];
        assert_eq!(at_menu_items_for("@r", &files).len(), 3);
        assert_eq!(at_menu_items_for("@", &files).len(), 3);
        assert_eq!(at_menu_items_for("@readme", &files).len(), 1);
        assert!(at_menu_items_for("@zzz", &files).is_empty());
    }

    #[test]
    fn apply_at_to_input_replaces_query() {
        assert_eq!(apply_at_to_input("@", "README.md"), "@README.md ");
        assert_eq!(apply_at_to_input("@read", "README.md"), "@README.md ");
        assert_eq!(
            apply_at_to_input("please check @read", "README.md"),
            "please check @README.md "
        );
    }
}
