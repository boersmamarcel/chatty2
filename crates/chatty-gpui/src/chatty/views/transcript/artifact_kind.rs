use std::path::{Path, PathBuf};

/// True when `path` looks like a PDF (extension only — we do not sniff bytes).
pub fn is_pdf_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
}

/// Source text for the artifact panel. PDFs are binary — return empty and let
/// the view render pages / extract text via pdfium.
pub fn read_artifact_source(path: &Path) -> String {
    if is_pdf_path(path) {
        String::new()
    } else {
        std::fs::read_to_string(path).unwrap_or_default()
    }
}

/// Pull a file path out of a tool's JSON-ish input (`path` / `file_path` / `filename`).
pub fn tool_file_path(input: &str) -> Option<PathBuf> {
    for key in ["path", "file_path", "filename"] {
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

/// Write/create tools, plus PDF tools that name a `.pdf` path.
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
    name.starts_with("pdf_") && tool_file_path(input).is_some_and(|p| is_pdf_path(&p))
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
        assert!(is_produced_file_tool(
            "pdf_extract_text",
            r#"{"path":"docs/report.pdf"}"#
        ));
        assert!(is_produced_file_tool("write_file", r#"{"path":"a.md"}"#));
        assert!(!is_produced_file_tool(
            "google_search",
            r#"{"query":"pdf"}"#
        ));
    }
}
