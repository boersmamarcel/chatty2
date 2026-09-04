use std::path::{Path, PathBuf};

pub fn is_tabular_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "csv" | "tsv" | "parquet"))
}

/// Raster/vector image paths the artifact panel can preview (charts, exports).
pub fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp"
            )
        })
}

/// Lane A browser tools (AGE-142/AGE-155). Matched by exact name rather than
/// a `browser_` prefix — `browser_use` is a distinct, unrelated tool.
pub fn is_lane_a_browser_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "browser_navigate"
            | "browser_snapshot"
            | "browser_screenshot"
            | "browser_console"
            | "browser_network"
            | "browser_resize"
    )
}

/// True when `path` looks like a PDF (extension only — we do not sniff bytes).
pub fn is_pdf_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
}

/// Attachment paths shown inline under a message bubble (images, charts).
/// PDFs use artifact cards in the typed transcript instead.
///
/// Duplicates are dropped, keeping first occurrence. One file can reach the
/// queue by more than one route — a tool that attaches its own output plus an
/// explicit `add_attachment` on the same path — and rendering it twice reads
/// as a bug to the user, not as two results.
pub fn inline_chat_attachments(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    paths
        .into_iter()
        .filter(|path| !is_pdf_path(path))
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

#[cfg(test)]
mod inline_attachment_tests {
    use super::*;

    #[test]
    fn drops_duplicate_paths() {
        let shot = PathBuf::from("/ws/.chatty/browser/shot.png");
        let out = inline_chat_attachments(vec![shot.clone(), shot.clone()]);
        assert_eq!(out, vec![shot], "the same image must render once");
    }

    #[test]
    fn keeps_distinct_images_in_order() {
        let a = PathBuf::from("/ws/a.png");
        let b = PathBuf::from("/ws/b.png");
        let out = inline_chat_attachments(vec![a.clone(), b.clone(), a.clone()]);
        assert_eq!(out, vec![a, b], "distinct screenshots each keep their slot");
    }

    #[test]
    fn still_excludes_pdfs() {
        let out = inline_chat_attachments(vec![
            PathBuf::from("/ws/report.pdf"),
            PathBuf::from("/ws/a.png"),
        ]);
        assert_eq!(out, vec![PathBuf::from("/ws/a.png")]);
    }
}

/// Syntax-highlighter language id for a workspace file (gpui-component names).
pub fn artifact_language_for_path(path: &Path) -> Option<String> {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())?;
    let lang = match ext.as_str() {
        "rs" => "rust",
        "py" => "python",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" => "typescript",
        "tsx" => "tsx",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => "cpp",
        "cs" => "csharp",
        "rb" => "ruby",
        "swift" => "swift",
        "sql" => "sql",
        "sh" | "bash" | "zsh" => "bash",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "json" | "jsonc" => "json",
        "html" | "htm" => "html",
        "css" | "scss" => "css",
        "md" | "mdx" => "markdown",
        "txt" => "plaintext",
        "zig" => "zig",
        "proto" => "proto",
        "graphql" | "gql" => "graphql",
        "ex" | "exs" => "elixir",
        "scala" => "scala",
        _ => return None,
    };
    Some(lang.to_string())
}

pub fn is_markdown_artifact_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "md" | "mdx"))
}

pub fn is_code_artifact_path(path: &Path) -> bool {
    !is_pdf_path(path)
        && !is_image_path(path)
        && !is_tabular_path(path)
        && !is_markdown_artifact_path(path)
        && artifact_language_for_path(path).is_some()
}

/// Images, PDFs, and tabular exports stay as full artifact cards — not batched receipts.
pub fn is_standalone_artifact_path(path: &Path) -> bool {
    is_image_path(path) || is_pdf_path(path) || is_tabular_path(path)
}

/// Deliverables that earn a transcript artifact receipt card.
pub fn is_transcript_artifact_receipt(path: &Path) -> bool {
    is_markdown_artifact_path(path)
        || is_pdf_path(path)
        || is_image_path(path)
        || is_tabular_path(path)
}

/// Previous file body for diff view when a tool carried it (e.g. `apply_diff`).
pub fn artifact_old_content_from_tool(
    tool: &chatty_core::models::message_types::ToolCallBlock,
) -> Option<String> {
    let name = tool.tool_name.to_ascii_lowercase();
    if !(name.contains("diff") || name.contains("apply") || name.contains("edit")) {
        return None;
    }
    let json = serde_json::from_str::<serde_json::Value>(&tool.input).ok()?;
    json.get("old_content")
        .or_else(|| json.get("old_string"))
        .or_else(|| json.get("old"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Path for UI display: relative to workspace when possible.
pub fn artifact_display_path(path: &Path, workspace: Option<&Path>) -> String {
    let resolved = resolve_artifact_path(path, workspace);
    if let Some(ws) = workspace.filter(|w| !w.as_os_str().is_empty())
        && let Ok(rel) = resolved.strip_prefix(ws)
    {
        let trimmed = rel
            .to_string_lossy()
            .trim_start_matches('/')
            .trim_start_matches('\\')
            .to_string();
        if !trimmed.is_empty() {
            return trimmed.replace('\\', "/");
        }
    }
    if path.is_relative() {
        return path.to_string_lossy().replace('\\', "/");
    }
    resolved.to_string_lossy().replace('\\', "/")
}

/// Resolve a tool-produced path against the workspace (and cwd). Relative
/// paths like `poem.md` otherwise fail to read and the Rendered tab is blank.
pub fn resolve_artifact_path(path: &Path, workspace: Option<&Path>) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    if let Some(ws) = workspace {
        let joined = ws.join(path);
        if joined.exists() {
            return joined;
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        let joined = cwd.join(path);
        if joined.exists() {
            return joined;
        }
    }
    path.to_path_buf()
}

/// Source text for the artifact panel. PDFs are binary — return empty and let
/// the view render pages / extract text via pdfium.
pub fn read_artifact_source(path: &Path) -> String {
    if is_pdf_path(path) || is_image_path(path) {
        String::new()
    } else {
        std::fs::read_to_string(path).unwrap_or_default()
    }
}

/// On-disk PNG from a successful `create_chart` — only when `saved_path` exists on disk.
/// Never use the input `save_path` alone; that path may never have been written.
pub fn chart_artifact_path(
    tool: &chatty_core::models::message_types::ToolCallBlock,
) -> Option<PathBuf> {
    if tool.tool_name != "create_chart" {
        return None;
    }
    let output = tool.output.as_deref()?;
    let json: serde_json::Value = serde_json::from_str(output).ok()?;
    let path = json.get("saved_path").and_then(|v| v.as_str())?;
    let path = PathBuf::from(path);
    if is_image_path(&path) && path.exists() {
        Some(path)
    } else {
        None
    }
}

/// True when a tool produced an image the artifact panel should preview.
pub fn is_chart_artifact_tool(tool_name: &str, _input: &str, output: Option<&str>) -> bool {
    if tool_name != "create_chart" {
        return false;
    }
    let Some(output) = output else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(output) else {
        return false;
    };
    let Some(path) = json.get("saved_path").and_then(|v| v.as_str()) else {
        return false;
    };
    is_image_path(Path::new(path)) && Path::new(path).exists()
}

/// Produced-file or chart tool whose result is an image path.
pub fn is_image_artifact_tool(tool_name: &str, input: &str, output: Option<&str>) -> bool {
    if is_chart_artifact_tool(tool_name, input, output) {
        return true;
    }
    if !is_produced_file_tool(tool_name, input) {
        return false;
    }
    tool_file_path(input)
        .or_else(|| output.and_then(tool_file_path))
        .is_some_and(|path| is_image_path(&path))
}

/// Image path from a successful `add_attachment` tool call.
pub fn attachment_image_path(
    tool: &chatty_core::models::message_types::ToolCallBlock,
) -> Option<PathBuf> {
    if tool.tool_name != "add_attachment" {
        return None;
    }
    let output = tool.output.as_deref()?;
    let json: serde_json::Value = serde_json::from_str(output).ok()?;
    let path = json.get("path").and_then(|v| v.as_str())?;
    let path = PathBuf::from(path);
    if is_image_path(&path) && path.exists() {
        Some(path)
    } else {
        None
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

/// Write/create tools, Typst PDF compile, and PDF tools that name a `.pdf` path.
///
/// Agent plan tools (`write_todos`, …) are not file artifacts — they own the
/// Plan block and must not auto-open the document panel.
pub fn is_produced_file_tool(tool_name: &str, input: &str) -> bool {
    let name = tool_name.to_ascii_lowercase();
    if name.contains("todo") || name == "verify_completion" {
        return false;
    }
    // Directory creation is an activity-row event, not a document artifact.
    if name == "create_directory" {
        return false;
    }
    if (name.contains("write") || name.contains("create")) && !name.contains("diff") {
        return true;
    }
    // Typst always writes a PDF via output_path / saved_path.
    if name == "compile_typst" || name.contains("typst") {
        return tool_file_path(input).is_some_and(|p| is_pdf_path(&p)) || name == "compile_typst";
    }
    name.starts_with("pdf_") && tool_file_path(input).is_some_and(|p| is_pdf_path(&p))
}

/// Verbatim basename. Never slugified — `agentic-chat-ui-gpui.md` stays that way.
pub fn artifact_file_name(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// Display title kept as a separate field from [`artifact_file_name`].
/// Default is the filename; do not title-case or unslugify.
pub fn artifact_display_title(path: &Path) -> String {
    artifact_file_name(path)
}

/// Format token (`MD`, `RS`, `CSV`, `PNG`, `PDF`, `FILE`).
pub fn artifact_format_token(path: &Path) -> String {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_uppercase())
        .unwrap_or_else(|| "FILE".to_string())
}

/// Type token (`Document`, `Code`, `Image`, `Data`).
pub fn artifact_type_token(path: &Path) -> &'static str {
    if is_tabular_path(path) {
        "Data"
    } else if is_image_path(path) || is_pdf_path(path) {
        "Image"
    } else if is_code_artifact_path(path) {
        "Code"
    } else {
        "Document"
    }
}

/// Exactly two muted tokens: `Document · MD`.
pub fn artifact_meta_line(path: &Path) -> String {
    format!(
        "{} · {}",
        artifact_type_token(path),
        artifact_format_token(path)
    )
}

/// Panel title, character-identical to the card filename plus `· MD`.
pub fn artifact_panel_title(path: &Path) -> String {
    format!(
        "{} · {}",
        artifact_file_name(path),
        artifact_format_token(path)
    )
}

/// First `n` source lines for the card peek popover.
pub fn artifact_peek_lines(source: &str, n: usize) -> String {
    source.lines().take(n).collect::<Vec<_>>().join("\n")
}

/// `(rows, columns)` from a CSV/TSV body. Rows count non-empty lines.
pub fn csv_shape(source: &str) -> Option<(usize, usize)> {
    let mut lines = source.lines().filter(|line| !line.trim().is_empty());
    let header = lines.next()?;
    let delim = if header.contains('\t') { '\t' } else { ',' };
    let columns = header.split(delim).count().max(1);
    let rows = 1 + lines.count();
    Some((rows, columns))
}

pub fn format_count(n: usize) -> String {
    let raw = n.to_string();
    let mut out = String::new();
    for (i, ch) in raw.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

pub fn csv_stat_line(source: &str) -> Option<String> {
    let (rows, cols) = csv_shape(source)?;
    Some(format!(
        "{} rows · {} columns",
        format_count(rows),
        format_count(cols)
    ))
}

/// On-disk identity used to detect an open panel going stale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArtifactVersion {
    pub mtime_secs: u64,
    pub len: u64,
}

pub fn artifact_version(path: &Path) -> Option<ArtifactVersion> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime_secs = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(ArtifactVersion {
        mtime_secs,
        len: meta.len(),
    })
}

/// Markdown ATX heading (`#`–`######`) with 0-based source line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactHeading {
    pub level: u8,
    pub title: String,
    pub line: u32,
}

pub fn markdown_headings(source: &str) -> Vec<ArtifactHeading> {
    source
        .lines()
        .enumerate()
        .filter_map(|(ix, line)| {
            let trimmed = line.trim_start();
            if !trimmed.starts_with('#') {
                return None;
            }
            let hashes = trimmed.bytes().take_while(|b| *b == b'#').count();
            if !(1..=6).contains(&hashes) {
                return None;
            }
            let rest = &trimmed[hashes..];
            if !rest.is_empty() && !rest.starts_with(' ') && !rest.starts_with('\t') {
                return None;
            }
            let title = rest.trim().trim_end_matches('#').trim();
            if title.is_empty() {
                return None;
            }
            Some(ArtifactHeading {
                level: hashes as u8,
                title: title.to_string(),
                line: ix as u32,
            })
        })
        .collect()
}

/// Content position for rendered↔source toggle. Prefer a heading over pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ViewAnchor {
    SourceLine(u32),
    BlockIndex(usize),
    ScrollFraction(f32),
}

/// Nearest heading at or before `line`.
pub fn heading_index_for_line(headings: &[ArtifactHeading], line: u32) -> Option<usize> {
    headings
        .iter()
        .enumerate()
        .rev()
        .find(|(_, heading)| heading.line <= line)
        .map(|(ix, _)| ix)
        .or_else(|| headings.first().map(|_| 0))
}

pub fn anchor_from_source_line(headings: &[ArtifactHeading], line: u32) -> ViewAnchor {
    match heading_index_for_line(headings, line) {
        Some(ix) => ViewAnchor::BlockIndex(ix),
        None => ViewAnchor::SourceLine(line),
    }
}

pub fn source_line_from_anchor(headings: &[ArtifactHeading], anchor: ViewAnchor) -> u32 {
    match anchor {
        ViewAnchor::SourceLine(line) => line,
        ViewAnchor::BlockIndex(ix) => headings.get(ix).map(|h| h.line).unwrap_or(0),
        ViewAnchor::ScrollFraction(fraction) => {
            if headings.is_empty() {
                return 0;
            }
            let ix = ((fraction.clamp(0.0, 1.0) * headings.len() as f32) as usize)
                .min(headings.len().saturating_sub(1));
            headings[ix].line
        }
    }
}

pub fn block_index_from_anchor(headings: &[ArtifactHeading], anchor: ViewAnchor) -> Option<usize> {
    match anchor {
        ViewAnchor::BlockIndex(ix) if ix < headings.len() => Some(ix),
        ViewAnchor::BlockIndex(_) => headings.len().checked_sub(1),
        ViewAnchor::SourceLine(line) => heading_index_for_line(headings, line),
        ViewAnchor::ScrollFraction(fraction) => {
            if headings.is_empty() {
                return None;
            }
            Some(
                ((fraction.clamp(0.0, 1.0) * headings.len() as f32) as usize)
                    .min(headings.len().saturating_sub(1)),
            )
        }
    }
}

/// True when a produced-file tool result is a PDF the artifact panel should open.
pub fn is_pdf_artifact_tool(tool_name: &str, input: &str, output: Option<&str>) -> bool {
    if !is_produced_file_tool(tool_name, input) {
        // compile_typst may only expose the absolute path on output.
        let name = tool_name.to_ascii_lowercase();
        if name != "compile_typst" && !name.contains("typst") {
            return false;
        }
    }
    tool_file_path(input)
        .or_else(|| output.and_then(tool_file_path))
        .is_some_and(|p| is_pdf_path(&p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn pdf_extension_is_case_insensitive() {
        assert!(is_pdf_path(Path::new("/tmp/Report.PDF")));
        assert!(is_pdf_path(Path::new("notes.pdf")));
        assert!(!is_pdf_path(Path::new("notes.md")));
        assert!(!is_pdf_path(Path::new("notes")));
    }

    #[test]
    fn image_extension_detection() {
        assert!(is_image_path(Path::new("charts/revenue.PNG")));
        assert!(is_image_path(Path::new("plot.svg")));
        assert!(!is_image_path(Path::new("data.csv")));
        assert!(!is_code_artifact_path(Path::new("chart.png")));
    }

    #[test]
    fn inline_chat_attachments_skip_pdfs() {
        let paths = inline_chat_attachments(vec![
            PathBuf::from("chart.png"),
            PathBuf::from("notes.pdf"),
            PathBuf::from("/tmp/report.PDF"),
        ]);
        assert_eq!(paths, vec![PathBuf::from("chart.png")]);
    }

    #[test]
    fn artifact_language_from_extension() {
        assert_eq!(
            artifact_language_for_path(Path::new("main.rs")).as_deref(),
            Some("rust")
        );
        assert_eq!(
            artifact_language_for_path(Path::new("app.py")).as_deref(),
            Some("python")
        );
        assert!(is_code_artifact_path(Path::new("lib.go")));
        assert!(is_code_artifact_path(Path::new("requirements.txt")));
        assert!(is_markdown_artifact_path(Path::new("README.md")));
    }

    #[test]
    fn artifact_old_content_from_apply_diff() {
        use chatty_core::models::message_types::{ToolCallBlock, ToolCallState, ToolSource};
        let tool = ToolCallBlock {
            id: "d1".into(),
            tool_name: "apply_diff".into(),
            display_name: "apply_diff".into(),
            input:
                r#"{"path":"src/main.rs","old_content":"fn old()\n","new_content":"fn new()\n"}"#
                    .into(),
            output: None,
            output_preview: None,
            state: ToolCallState::Success,
            duration: None,
            text_before: String::new(),
            source: ToolSource::Local,
            execution_engine: None,
        };
        assert_eq!(
            artifact_old_content_from_tool(&tool).as_deref(),
            Some("fn old()\n")
        );
    }

    #[test]
    fn read_source_skips_binary_pdfs() {
        let dir = std::env::temp_dir();
        let pdf = dir.join("artifact_kind_skip.pdf");
        let mut file = std::fs::File::create(&pdf).expect("create pdf");
        file.write_all(b"%PDF-1.4 binary\x00\xff").expect("write");
        drop(file);
        assert_eq!(read_artifact_source(&pdf), "");
        let _ = std::fs::remove_file(&pdf);
    }

    #[test]
    fn resolve_relative_path_against_workspace() {
        let dir = std::env::temp_dir().join("artifact_kind_resolve_ws");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("poem.md");
        std::fs::write(&file, "# hi").expect("write");
        let resolved = resolve_artifact_path(Path::new("poem.md"), Some(&dir));
        assert_eq!(resolved, file);
        assert_eq!(read_artifact_source(&resolved), "# hi");
        assert_eq!(artifact_display_path(&file, Some(&dir)), "poem.md");
        assert_eq!(
            artifact_display_path(&dir.join("docs/report.md"), Some(&dir),),
            "docs/report.md"
        );
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn read_source_loads_text_files() {
        let dir = std::env::temp_dir();
        let md = dir.join("artifact_kind_read.md");
        std::fs::write(&md, "# hello").expect("write md");
        assert_eq!(read_artifact_source(&md), "# hello");
        let _ = std::fs::remove_file(&md);
    }

    #[test]
    fn tool_path_and_pdf_tools() {
        assert_eq!(
            tool_file_path(r#"{"path":"docs/report.pdf"}"#),
            Some(PathBuf::from("docs/report.pdf"))
        );
        assert_eq!(
            tool_file_path(r#"{"output_path":"reports/sales.pdf"}"#),
            Some(PathBuf::from("reports/sales.pdf"))
        );
        assert_eq!(
            tool_file_path(r#"{"saved_path":"/tmp/out.pdf","page_count":2}"#),
            Some(PathBuf::from("/tmp/out.pdf"))
        );
        assert!(is_produced_file_tool(
            "pdf_extract_text",
            r#"{"path":"docs/report.pdf"}"#
        ));
        assert!(is_produced_file_tool(
            "compile_typst",
            r#"{"content":"= Hi","output_path":"out.pdf"}"#
        ));
        assert!(is_pdf_artifact_tool(
            "compile_typst",
            r#"{"content":"= Hi","output_path":"out.pdf"}"#,
            Some(r#"{"saved_path":"/abs/out.pdf","page_count":1}"#)
        ));
        assert!(is_produced_file_tool("write_file", r#"{"path":"a.md"}"#));
        assert!(!is_produced_file_tool(
            "create_directory",
            r#"{"path":"src/components"}"#
        ));
        assert!(!is_produced_file_tool(
            "google_search",
            r#"{"query":"pdf"}"#
        ));
        assert!(!is_pdf_artifact_tool(
            "write_file",
            r#"{"path":"a.md"}"#,
            None
        ));
    }

    #[test]
    fn chart_artifact_from_saved_path() {
        use chatty_core::models::message_types::{ToolCallBlock, ToolCallState, ToolSource};
        let dir = std::env::temp_dir().join("artifact_kind_chart_png");
        let _ = std::fs::create_dir_all(&dir);
        let png = dir.join("sales.png");
        std::fs::write(&png, b"\x89PNG").expect("write png");
        let tool = ToolCallBlock {
            id: "c1".into(),
            tool_name: "create_chart".into(),
            display_name: "Creating chart".into(),
            input: r#"{"chart_type":"bar","save_path":"charts/sales.png","data":[]}"#.into(),
            output: Some(format!(
                r#"{{"chart_type":"bar","data":[],"saved_path":"{}"}}"#,
                png.display()
            )),
            output_preview: None,
            state: ToolCallState::Success,
            duration: None,
            text_before: String::new(),
            source: ToolSource::Local,
            execution_engine: None,
        };
        assert_eq!(chart_artifact_path(&tool), Some(png.clone()));
        assert!(is_chart_artifact_tool(
            "create_chart",
            &tool.input,
            tool.output.as_deref()
        ));
        assert!(is_image_artifact_tool(
            "create_chart",
            &tool.input,
            tool.output.as_deref()
        ));
        assert!(is_image_artifact_tool(
            "write_file",
            r#"{"path":"out/chart.png"}"#,
            None
        ));
        // Missing file must not become an artifact path.
        let missing = ToolCallBlock {
            output: Some(
                r#"{"chart_type":"bar","data":[],"saved_path":"/app/charts/missing.png"}"#.into(),
            ),
            ..tool
        };
        assert_eq!(chart_artifact_path(&missing), None);
        let _ = std::fs::remove_file(&png);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn display_title_is_not_unslugified() {
        let path = Path::new("docs/agentic-chat-ui-gpui.md");
        assert_eq!(artifact_file_name(path), "agentic-chat-ui-gpui.md");
        assert_eq!(artifact_display_title(path), "agentic-chat-ui-gpui.md");
        assert_eq!(artifact_meta_line(path), "Document · MD");
        assert_eq!(artifact_panel_title(path), "agentic-chat-ui-gpui.md · MD");
        assert_eq!(artifact_type_token(Path::new("lib.rs")), "Code");
        assert_eq!(
            artifact_meta_line(Path::new("requirements.txt")),
            "Code · TXT"
        );
        assert_eq!(artifact_type_token(Path::new("sales.csv")), "Data");
        assert_eq!(artifact_type_token(Path::new("plot.png")), "Image");
    }

    #[test]
    fn csv_shape_and_peek() {
        let csv = "a,b,c\n1,2,3\n4,5,6\n\n";
        assert_eq!(csv_shape(csv), Some((3, 3)));
        assert_eq!(csv_stat_line(csv).as_deref(), Some("3 rows · 3 columns"));
        assert_eq!(format_count(1240), "1,240");
        assert_eq!(artifact_peek_lines("one\ntwo\nthree\nfour", 2), "one\ntwo");
    }

    #[test]
    fn headings_and_toggle_anchor_stay_on_same_heading() {
        let source = "# Intro\n\npara\n## Details\nmore\n# End\n";
        let headings = markdown_headings(source);
        assert_eq!(headings.len(), 3);
        assert_eq!(headings[1].title, "Details");
        assert_eq!(headings[1].line, 3);

        let mut anchor = ViewAnchor::BlockIndex(1);
        for _ in 0..3 {
            let line = source_line_from_anchor(&headings, anchor);
            assert_eq!(line, 3);
            anchor = anchor_from_source_line(&headings, line);
        }
        assert_eq!(anchor, ViewAnchor::BlockIndex(1));
        assert_eq!(heading_index_for_line(&headings, 4), Some(1));
        assert_eq!(
            block_index_from_anchor(&headings, ViewAnchor::SourceLine(4)),
            Some(1)
        );
    }

    #[test]
    fn opening_another_path_returns_to_docked() {
        use super::super::artifact_view::{ArtifactMode, presentation_on_open};
        assert_eq!(
            presentation_on_open(ArtifactMode::Closed, false),
            ArtifactMode::Docked
        );
        assert_eq!(
            presentation_on_open(ArtifactMode::Full, false),
            ArtifactMode::Docked
        );
        assert_eq!(
            presentation_on_open(ArtifactMode::Full, true),
            ArtifactMode::Full
        );
        assert_eq!(
            presentation_on_open(ArtifactMode::Docked, false),
            ArtifactMode::Docked
        );
    }

    #[test]
    fn transcript_artifact_receipt_allowlist() {
        assert!(is_transcript_artifact_receipt(Path::new("README.md")));
        assert!(is_transcript_artifact_receipt(Path::new("report.pdf")));
        assert!(is_transcript_artifact_receipt(Path::new("chart.png")));
        assert!(is_transcript_artifact_receipt(Path::new("data.csv")));
        assert!(!is_transcript_artifact_receipt(Path::new("index.html")));
        assert!(!is_transcript_artifact_receipt(Path::new("main.rs")));
        assert!(!is_transcript_artifact_receipt(Path::new("app.py")));
    }

    #[test]
    fn artifact_version_changes_when_file_rewritten() {
        let path = std::env::temp_dir().join("artifact_kind_version.md");
        std::fs::write(&path, "v1").expect("write");
        let first = artifact_version(&path).expect("stat");
        std::fs::write(&path, "v2-longer").expect("rewrite");
        let second = artifact_version(&path).expect("stat");
        assert_ne!(first.len, second.len);
        let _ = std::fs::remove_file(&path);
    }
}
