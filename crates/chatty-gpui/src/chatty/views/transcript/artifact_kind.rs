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

/// True when `path` looks like a PDF (extension only — we do not sniff bytes).
pub fn is_pdf_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
}

/// Attachment paths shown inline under a message bubble (images, charts).
/// PDFs use artifact cards in the typed transcript instead.
pub fn inline_chat_attachments(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    paths
        .into_iter()
        .filter(|path| !is_pdf_path(path))
        .collect()
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
    if (name.contains("write") || name.contains("create")) && !name.contains("diff") {
        return true;
    }
    // Typst always writes a PDF via output_path / saved_path.
    if name == "compile_typst" || name.contains("typst") {
        return tool_file_path(input).is_some_and(|p| is_pdf_path(&p)) || name == "compile_typst";
    }
    name.starts_with("pdf_") && tool_file_path(input).is_some_and(|p| is_pdf_path(&p))
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
        assert!(is_markdown_artifact_path(Path::new("README.md")));
        assert!(!is_code_artifact_path(Path::new("notes.txt")));
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
}
