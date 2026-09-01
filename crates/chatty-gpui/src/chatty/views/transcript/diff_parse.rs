/// Parsed unified diff used by [`super::DiffHunkList`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnifiedDiff {
    pub path: String,
    pub hunk: String,
    pub old: String,
    pub new: String,
}

/// Split a unified diff into path, first hunk header, and old/new bodies.
pub fn parse_unified_diff(input: &str) -> Option<UnifiedDiff> {
    let text = input.trim();
    if !text.contains("@@") && !text.contains("\n+") && !text.contains("\n-") {
        return None;
    }
    let mut path = String::new();
    let mut hunk = String::new();
    let mut old = String::new();
    let mut new = String::new();
    let mut in_hunk = false;

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            path = rest.trim().to_string();
            continue;
        }
        if path.is_empty()
            && let Some(rest) = line.strip_prefix("+++ ")
        {
            path = rest.trim().trim_start_matches("b/").to_string();
            continue;
        }
        if line.starts_with("@@") {
            if hunk.is_empty() {
                hunk = line.trim().to_string();
            }
            in_hunk = true;
            continue;
        }
        if !in_hunk {
            continue;
        }
        if let Some(rest) = line.strip_prefix('+') {
            new.push_str(rest);
            new.push('\n');
        } else if let Some(rest) = line.strip_prefix('-') {
            old.push_str(rest);
            old.push('\n');
        } else {
            let body = line.strip_prefix(' ').unwrap_or(line);
            old.push_str(body);
            old.push('\n');
            new.push_str(body);
            new.push('\n');
        }
    }

    if old.is_empty() && new.is_empty() {
        return None;
    }
    Some(UnifiedDiff {
        path,
        hunk,
        old,
        new,
    })
}

/// Split a path into muted directory + emphasised basename.
pub fn split_path(path: &str) -> (String, String) {
    match path.rfind('/') {
        Some(idx) => (path[..=idx].to_string(), path[idx + 1..].to_string()),
        None => (String::new(), path.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_readme_changelog_hunk() {
        let raw =
            "--- a/README.md\n+++ b/README.md\n@@ -1,1 +1,3 @@\n # Chatty\n+\n+## Changelog\n";
        let parsed = parse_unified_diff(raw).expect("unified");
        assert_eq!(parsed.path, "README.md");
        assert!(parsed.hunk.contains("@@"));
        assert!(parsed.old.contains("# Chatty"));
        assert!(parsed.new.contains("## Changelog"));
    }

    #[test]
    fn split_path_emphasises_basename() {
        let (dir, base) = split_path(".github/workflows/ship-auto-merge.yml");
        assert_eq!(dir, ".github/workflows/");
        assert_eq!(base, "ship-auto-merge.yml");
    }
}
