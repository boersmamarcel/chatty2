use std::ops::Range;

use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::highlighter::SyntaxHighlighter;
use ropey::Rope;

/// Highlight code using gpui-component's tree-sitter-based syntax highlighter.
///
/// Returns range-based highlight styles (byte offsets into `code` paired with
/// GPUI `HighlightStyle`). Regions not covered by any range render with the
/// theme foreground. Returns an empty vec for unsupported or unspecified languages.
pub fn highlight_code(
    code: &str,
    language: Option<&str>,
    cx: &App,
) -> Vec<(Range<usize>, HighlightStyle)> {
    match language {
        Some(lang) => {
            let mut highlighter = SyntaxHighlighter::new(lang);
            let rope = Rope::from_str(code);
            highlighter.update(None, &rope);
            let theme = &cx.theme().highlight_theme;
            highlighter.styles(&(0..code.len()), theme)
        }
        None => vec![],
    }
}
