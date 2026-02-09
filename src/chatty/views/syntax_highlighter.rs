use gpui::*;
use gpui_component::ActiveTheme;
use once_cell::sync::Lazy;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

/// Global syntax set for language definitions (initialized once)
static SYNTAX_SET: Lazy<SyntaxSet> = Lazy::new(SyntaxSet::load_defaults_newlines);

/// Global theme set for syntax highlighting themes (initialized once)
static THEME_SET: Lazy<ThemeSet> = Lazy::new(ThemeSet::load_defaults);

/// A span of highlighted text with styling information
#[derive(Clone, Debug)]
pub struct HighlightedSpan {
    pub text: String,
    pub color: Hsla,
}

/// Normalize language names to match syntect's syntax definitions
fn normalize_language(lang: &str) -> String {
    match lang.to_lowercase().as_str() {
        "py" => "python".to_string(),
        "js" => "javascript".to_string(),
        "ts" => "typescript".to_string(),
        "rs" => "rust".to_string(),
        "sh" => "bash".to_string(),
        "yml" => "yaml".to_string(),
        "md" => "markdown".to_string(),
        "cpp" | "c++" => "c++".to_string(),
        "cs" => "c#".to_string(),
        "rb" => "ruby".to_string(),
        "golang" => "go".to_string(),
        other => other.to_string(),
    }
}

/// Convert syntect RGB color to GPUI Hsla color
fn syntect_color_to_hsla(color: syntect::highlighting::Color) -> Hsla {
    let r = color.r as f32 / 255.0;
    let g = color.g as f32 / 255.0;
    let b = color.b as f32 / 255.0;
    let a = color.a as f32 / 255.0;

    Hsla::from(Rgba { r, g, b, a })
}

/// Get the appropriate syntect theme based on GPUI's current theme mode
fn get_syntect_theme_name(cx: &App) -> &'static str {
    if cx.theme().mode.is_dark() {
        "Solarized (dark)"
    } else {
        "Solarized (light)"
    }
}

/// Highlight code and return a vector of styled spans
pub fn highlight_code(code: &str, language: Option<&str>, cx: &App) -> Vec<HighlightedSpan> {
    let mut spans = Vec::new();

    // Get the syntax definition for the language
    let syntax = if let Some(lang_name) = language {
        let normalized = normalize_language(lang_name);
        SYNTAX_SET
            .find_syntax_by_extension(&normalized)
            .or_else(|| SYNTAX_SET.find_syntax_by_name(&normalized))
            .or_else(|| SYNTAX_SET.find_syntax_by_token(&normalized))
    } else {
        None
    };

    // If no syntax found, return plain text with foreground color
    let Some(syntax) = syntax else {
        let foreground = cx.theme().foreground;
        spans.push(HighlightedSpan {
            text: code.to_string(),
            color: foreground,
        });
        return spans;
    };

    // Get the theme
    let theme_name = get_syntect_theme_name(cx);
    let theme = &THEME_SET.themes[theme_name];

    // Highlight the code line by line
    let mut highlighter = HighlightLines::new(syntax, theme);

    for line in LinesWithEndings::from(code) {
        let ranges = highlighter
            .highlight_line(line, &SYNTAX_SET)
            .unwrap_or_default();

        for (style, text) in ranges {
            let color = syntect_color_to_hsla(style.foreground);
            spans.push(HighlightedSpan {
                text: text.to_string(),
                color,
            });
        }
    }

    spans
}
