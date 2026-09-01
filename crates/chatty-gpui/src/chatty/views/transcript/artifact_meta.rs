//! Pure helpers for artifact cards and the document panel: labels, headings,
//! scroll anchors, staleness, and file-manager reveal.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::artifact_kind::{is_code_artifact_path, is_image_path, is_pdf_path, is_tabular_path};

/// View-mode tabs in the document panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArtifactViewMode {
    Rendered,
    Source,
    Diff,
}

/// Content-relative scroll position used when toggling rendered ↔ source.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ViewAnchor {
    SourceLine(u32),
    BlockIndex(usize),
    ScrollFraction(f32),
}

/// One ATX heading in a markdown document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heading {
    pub level: u8,
    pub text: String,
    pub source_line: u32,
}

/// Disk snapshot used to detect an agent rewrite while the panel is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArtifactVersion {
    pub modified: Option<SystemTime>,
    pub len: u64,
}

/// Basename shown verbatim (never slugified).
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// Display title is a separate field from [`file_name`]. For ordinary files
/// it matches the basename so `GPUI` is not lost to un-slugifying.
pub fn display_title(path: &Path, override_title: Option<&str>) -> String {
    override_title
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| file_name(path))
}

/// Type token for the card/panel meta line (`Document`, `Image`, `Data`, …).
pub fn type_token(path: &Path) -> &'static str {
    if is_pdf_path(path) {
        "Document"
    } else if is_image_path(path) {
        "Image"
    } else if is_tabular_path(path) {
        "Data"
    } else if is_code_artifact_path(path) {
        "Code"
    } else {
        "Document"
    }
}

/// Format token (`MD`, `RS`, `CSV`, `PDF`, …).
pub fn format_token(path: &Path) -> String {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_uppercase())
        .unwrap_or_else(|| "FILE".to_string())
}

/// Exactly two tokens: `Document · MD`.
pub fn card_meta(path: &Path) -> String {
    format!("{} · {}", type_token(path), format_token(path))
}

/// Title character-identical to the card, including `· MD`.
pub fn panel_title(path: &Path, override_title: Option<&str>) -> String {
    format!(
        "{} · {}",
        display_title(path, override_title),
        format_token(path)
    )
}

/// First ~20 lines for a peek tooltip (text artifacts only).
pub fn peek_lines(source: &str, max_lines: usize) -> String {
    source
        .lines()
        .take(max_lines)
        .collect::<Vec<_>>()
        .join("\n")
}

/// `(rows, cols)` for CSV/TSV. Rows count the header.
pub fn tabular_shape(text: &str, path: &Path) -> Option<(usize, usize)> {
    if !is_tabular_path(path) {
        return None;
    }
    let delim = if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("tsv"))
    {
        '\t'
    } else {
        ','
    };
    let mut rows = 0usize;
    let mut cols = 0usize;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        rows += 1;
        if rows == 1 {
            cols = line.split(delim).count();
        }
    }
    (rows > 0).then_some((rows, cols))
}

pub fn format_tabular_shape(rows: usize, cols: usize) -> String {
    format!("{} rows · {} columns", format_count(rows), cols)
}

fn format_count(n: usize) -> String {
    if n < 1000 {
        n.to_string()
    } else {
        let mut s = String::new();
        let raw = n.to_string();
        for (i, ch) in raw.chars().enumerate() {
            if i > 0 && (raw.len() - i).is_multiple_of(3) {
                s.push(',');
            }
            s.push(ch);
        }
        s
    }
}

/// Parse ATX headings, skipping fenced code.
pub fn parse_headings(source: &str) -> Vec<Heading> {
    let mut headings = Vec::new();
    let mut in_fence = false;
    for (i, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if !trimmed.starts_with('#') {
            continue;
        }
        let level = trimmed.chars().take_while(|c| *c == '#').count();
        if !(1..=6).contains(&level) {
            continue;
        }
        let after = &trimmed[level..];
        if after.is_empty() || !(after.starts_with(' ') || after.starts_with('\t')) {
            continue;
        }
        let text = after.trim().trim_end_matches('#').trim().to_string();
        if text.is_empty() {
            continue;
        }
        headings.push(Heading {
            level: level as u8,
            text,
            source_line: i as u32,
        });
    }
    headings
}

pub fn heading_index_for_line(headings: &[Heading], line: u32) -> usize {
    match headings.iter().rposition(|h| h.source_line <= line) {
        Some(idx) => idx,
        None => 0,
    }
}

/// Capture a content-relative anchor from a pixel scroll fraction.
pub fn capture_anchor(
    mode: ArtifactViewMode,
    scroll_fraction: f32,
    headings: &[Heading],
    line_count: u32,
) -> ViewAnchor {
    let frac = scroll_fraction.clamp(0.0, 1.0);
    match mode {
        ArtifactViewMode::Source => {
            let last = line_count.saturating_sub(1);
            let line = (frac * line_count.max(1) as f32).floor() as u32;
            ViewAnchor::SourceLine(line.min(last))
        }
        ArtifactViewMode::Rendered => {
            if headings.is_empty() {
                ViewAnchor::ScrollFraction(frac)
            } else {
                let idx = ((frac * headings.len() as f32).floor() as usize)
                    .min(headings.len().saturating_sub(1));
                ViewAnchor::BlockIndex(idx)
            }
        }
        ArtifactViewMode::Diff => ViewAnchor::ScrollFraction(frac),
    }
}

/// Restore a scroll fraction for `mode` from a previously captured anchor.
pub fn restore_fraction(
    anchor: ViewAnchor,
    mode: ArtifactViewMode,
    headings: &[Heading],
    line_count: u32,
) -> f32 {
    match (anchor, mode) {
        (ViewAnchor::SourceLine(line), ArtifactViewMode::Rendered) => {
            if headings.is_empty() {
                return if line_count == 0 {
                    0.0
                } else {
                    line as f32 / line_count as f32
                };
            }
            let idx = heading_index_for_line(headings, line);
            idx as f32 / headings.len() as f32
        }
        (ViewAnchor::BlockIndex(idx), ArtifactViewMode::Source) => {
            let line = headings.get(idx).map(|h| h.source_line).unwrap_or(0);
            if line_count == 0 {
                0.0
            } else {
                line as f32 / line_count as f32
            }
        }
        (ViewAnchor::SourceLine(line), ArtifactViewMode::Source) => {
            if line_count == 0 {
                0.0
            } else {
                line as f32 / line_count as f32
            }
        }
        (ViewAnchor::BlockIndex(idx), ArtifactViewMode::Rendered) => {
            if headings.is_empty() {
                0.0
            } else {
                idx.min(headings.len() - 1) as f32 / headings.len() as f32
            }
        }
        (ViewAnchor::ScrollFraction(f), _) => f.clamp(0.0, 1.0),
        (_, ArtifactViewMode::Diff) => match anchor {
            ViewAnchor::ScrollFraction(f) => f.clamp(0.0, 1.0),
            _ => 0.0,
        },
    }
}

pub fn current_version(path: &Path) -> ArtifactVersion {
    match std::fs::metadata(path) {
        Ok(meta) => ArtifactVersion {
            modified: meta.modified().ok(),
            len: meta.len(),
        },
        Err(_) => ArtifactVersion {
            modified: None,
            len: 0,
        },
    }
}

pub fn is_stale(loaded: ArtifactVersion, current: ArtifactVersion) -> bool {
    loaded != current
}

/// Full-window is a posture for the current document. Opening a different
/// artifact always returns to the docked workbench width.
pub fn keep_full_when_opening_other() -> bool {
    false
}

/// Reveal `path` in Finder / Explorer / the desktop file manager.
pub fn reveal_in_file_manager(path: &Path) {
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        let _ = reveal_command(&path);
    });
}

fn reveal_command(path: &Path) -> std::io::Result<std::process::ExitStatus> {
    #[cfg(target_os = "macos")]
    {
        return std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .status();
    }
    #[cfg(target_os = "windows")]
    {
        return std::process::Command::new("explorer")
            .arg("/select,")
            .arg(path)
            .status();
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let parent = path.parent().unwrap_or(path);
        std::process::Command::new("xdg-open").arg(parent).status()
    }
}

/// Nested outline items: `(id = source_line, label, children)`.
pub fn outline_tree(headings: &[Heading]) -> Vec<(String, String, Vec<(String, String)>)> {
    // Flattened parent/child pairs for tests; the view builds TreeItem from this.
    let mut roots: Vec<(String, String, Vec<(String, String)>)> = Vec::new();
    let mut current_root: Option<usize> = None;
    for h in headings {
        let id = h.source_line.to_string();
        if h.level <= 1 {
            roots.push((id, h.text.clone(), Vec::new()));
            current_root = Some(roots.len() - 1);
        } else if let Some(idx) = current_root {
            roots[idx].2.push((id, h.text.clone()));
        } else {
            roots.push((id, h.text.clone(), Vec::new()));
            current_root = Some(roots.len() - 1);
        }
    }
    roots
}

pub fn workspace_relative_path(path: &Path, workspace: Option<&Path>) -> PathBuf {
    workspace
        .and_then(|root| path.strip_prefix(root).ok())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "\
# Intro

hello

## Setup

details

## Usage

more

# Appendix

end
";

    #[test]
    fn file_name_is_verbatim_not_slugified() {
        let path = Path::new("docs/agentic-chat-ui-gpui.md");
        assert_eq!(file_name(path), "agentic-chat-ui-gpui.md");
        assert_eq!(display_title(path, None), "agentic-chat-ui-gpui.md");
        assert_eq!(display_title(path, Some("Release notes")), "Release notes");
        assert_eq!(card_meta(path), "Document · MD");
        assert_eq!(panel_title(path, None), "agentic-chat-ui-gpui.md · MD");
    }

    #[test]
    fn type_tokens_split_by_kind() {
        assert_eq!(type_token(Path::new("a.rs")), "Code");
        assert_eq!(format_token(Path::new("a.rs")), "RS");
        assert_eq!(card_meta(Path::new("plot.png")), "Image · PNG");
        assert_eq!(card_meta(Path::new("data.csv")), "Data · CSV");
        assert_eq!(card_meta(Path::new("notes.pdf")), "Document · PDF");
    }

    #[test]
    fn parse_headings_skips_fences_and_requires_space() {
        let src = "\
# Real

```
# not a heading
```

#NotHeading
## Nested
";
        let heads = parse_headings(src);
        assert_eq!(heads.len(), 2);
        assert_eq!(heads[0].text, "Real");
        assert_eq!(heads[1].text, "Nested");
        assert_eq!(heads[1].level, 2);
        assert_eq!(heads[1].source_line, 7);
    }

    #[test]
    fn toggle_rendered_source_thrice_keeps_the_same_heading() {
        let headings = parse_headings(DOC);
        assert_eq!(headings.len(), 4);
        let line_count = DOC.lines().count() as u32;
        let start_idx = 2; // "Usage"
        let mut mode = ArtifactViewMode::Rendered;
        let mut anchor = ViewAnchor::BlockIndex(start_idx);
        for _ in 0..3 {
            let next = match mode {
                ArtifactViewMode::Rendered => ArtifactViewMode::Source,
                _ => ArtifactViewMode::Rendered,
            };
            let frac = restore_fraction(anchor, next, &headings, line_count);
            anchor = capture_anchor(next, frac, &headings, line_count);
            mode = next;
        }
        assert_eq!(mode, ArtifactViewMode::Source);
        match anchor {
            ViewAnchor::SourceLine(line) => {
                assert_eq!(heading_index_for_line(&headings, line), start_idx);
            }
            other => panic!("expected SourceLine, got {other:?}"),
        }
    }

    #[test]
    fn tabular_shape_counts_rows_and_cols() {
        let csv = "a,b,c\n1,2,3\n4,5,6\n";
        assert_eq!(tabular_shape(csv, Path::new("t.csv")), Some((3, 3)));
        assert_eq!(format_tabular_shape(1240, 8), "1,240 rows · 8 columns");
        assert!(tabular_shape(csv, Path::new("t.md")).is_none());
    }

    #[test]
    fn stale_when_mtime_or_len_changes() {
        let a = ArtifactVersion {
            modified: None,
            len: 10,
        };
        let b = ArtifactVersion {
            modified: None,
            len: 11,
        };
        assert!(is_stale(a, b));
        assert!(!is_stale(a, a));
    }

    #[test]
    fn peek_truncates_to_max_lines() {
        let src = (0..30)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let peek = peek_lines(&src, 20);
        assert_eq!(peek.lines().count(), 20);
        assert!(peek.starts_with("line 0"));
        assert!(peek.contains("line 19"));
        assert!(!peek.contains("line 20"));
    }

    #[test]
    fn outline_nests_h2_under_h1() {
        let headings = parse_headings(DOC);
        let tree = outline_tree(&headings);
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].1, "Intro");
        assert_eq!(tree[0].2.len(), 2);
        assert_eq!(tree[0].2[1].1, "Usage");
        assert_eq!(tree[1].1, "Appendix");
    }

    #[test]
    fn opening_second_artifact_drops_full_is_a_mode_rule() {
        assert!(!keep_full_when_opening_other());
    }

    #[test]
    fn panel_title_matches_card_meta_format() {
        let path = Path::new("RELEASE_PROCESS.md");
        assert_eq!(card_meta(path), "Document · MD");
        assert_eq!(panel_title(path, None), "RELEASE_PROCESS.md · MD");
    }
}
