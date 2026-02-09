use super::syntax_highlighter;
use gpui::Hsla;

/// A single span within a line, preserving text and its associated color.
/// Used as an intermediate representation between highlighting and rendering.
#[derive(Clone, Debug, PartialEq)]
pub struct LineSpan {
    pub text: String,
    pub color: Hsla,
}

/// Split highlighted spans into lines, grouping spans that belong to the same line.
///
/// Each `HighlightedSpan` may contain newlines. This function splits them
/// so that each returned `Vec<LineSpan>` represents exactly one visual line.
pub fn split_spans_into_lines(
    spans: Vec<syntax_highlighter::HighlightedSpan>,
) -> Vec<Vec<LineSpan>> {
    let mut lines: Vec<Vec<LineSpan>> = Vec::new();
    let mut current_line: Vec<LineSpan> = Vec::new();

    for span in spans {
        // Split span by newlines
        let parts: Vec<&str> = span.text.split('\n').collect();

        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                // We hit a newline, push current line and start new one
                lines.push(std::mem::take(&mut current_line));
            }

            if !part.is_empty() {
                current_line.push(LineSpan {
                    text: part.to_string(),
                    color: span.color,
                });
            }
        }
    }

    // Push final line if any
    if !current_line.is_empty() {
        lines.push(current_line);
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Hsla;

    fn span(text: &str, h: f32) -> syntax_highlighter::HighlightedSpan {
        syntax_highlighter::HighlightedSpan {
            text: text.to_string(),
            color: Hsla {
                h,
                s: 1.0,
                l: 0.5,
                a: 1.0,
            },
        }
    }

    fn ls(text: &str, h: f32) -> LineSpan {
        LineSpan {
            text: text.to_string(),
            color: Hsla {
                h,
                s: 1.0,
                l: 0.5,
                a: 1.0,
            },
        }
    }

    #[test]
    fn test_empty_spans() {
        let result = split_spans_into_lines(vec![]);
        assert_eq!(result, Vec::<Vec<LineSpan>>::new());
    }

    #[test]
    fn test_single_line_single_span() {
        let result = split_spans_into_lines(vec![span("hello", 0.0)]);
        assert_eq!(result, vec![vec![ls("hello", 0.0)]]);
    }

    #[test]
    fn test_single_line_multiple_spans() {
        let result = split_spans_into_lines(vec![span("hello ", 0.0), span("world", 0.5)]);
        assert_eq!(result, vec![vec![ls("hello ", 0.0), ls("world", 0.5)]]);
    }

    #[test]
    fn test_multiple_lines_from_single_span() {
        let result = split_spans_into_lines(vec![span("line1\nline2\nline3", 0.0)]);
        assert_eq!(
            result,
            vec![
                vec![ls("line1", 0.0)],
                vec![ls("line2", 0.0)],
                vec![ls("line3", 0.0)],
            ]
        );
    }

    #[test]
    fn test_newline_at_end() {
        // "line1\n" splits into ["line1", ""] - trailing empty part is skipped
        let result = split_spans_into_lines(vec![span("line1\n", 0.0)]);
        assert_eq!(result, vec![vec![ls("line1", 0.0)]]);
    }

    #[test]
    fn test_newline_at_start() {
        // "\nline2" splits into ["", "line2"]
        // i=0: part="" -> skip. i=1: i>0 so push current_line (empty). part="line2" added.
        // After loop: current_line=[ls("line2")] -> pushed.
        // Result: [[], [ls("line2")]]
        let result = split_spans_into_lines(vec![span("\nline2", 0.0)]);
        assert_eq!(result, vec![vec![], vec![ls("line2", 0.0)]]);
    }

    #[test]
    fn test_only_newlines() {
        // "\n\n\n" splits into ["", "", "", ""]
        // Produces 3 empty lines (one per newline), final empty current_line not pushed
        let result = split_spans_into_lines(vec![span("\n\n\n", 0.0)]);
        assert_eq!(result, vec![vec![], vec![], vec![]]);
    }

    #[test]
    fn test_spans_crossing_lines() {
        let result = split_spans_into_lines(vec![
            span("let x", 0.0),
            span(" = 5;\nlet y", 0.3),
            span(" = 10;", 0.0),
        ]);
        assert_eq!(
            result,
            vec![
                vec![ls("let x", 0.0), ls(" = 5;", 0.3)],
                vec![ls("let y", 0.3), ls(" = 10;", 0.0)],
            ]
        );
    }

    #[test]
    fn test_empty_string_span() {
        let result = split_spans_into_lines(vec![span("", 0.0)]);
        assert_eq!(result, Vec::<Vec<LineSpan>>::new());
    }

    #[test]
    fn test_very_long_line() {
        let long_text = "x".repeat(3000);
        let result = split_spans_into_lines(vec![span(&long_text, 0.0)]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0][0].text.len(), 3000);
    }

    #[test]
    fn test_multiple_empty_lines_between_content() {
        // "a\n\n\nb" splits into ["a", "", "", "b"] -> 3 newlines produce 3 line breaks
        let result = split_spans_into_lines(vec![span("a\n\n\nb", 0.0)]);
        assert_eq!(
            result,
            vec![vec![ls("a", 0.0)], vec![], vec![], vec![ls("b", 0.0)],]
        );
    }

    #[test]
    fn test_single_character_lines() {
        let result = split_spans_into_lines(vec![span("a\nb\nc", 0.0)]);
        assert_eq!(
            result,
            vec![vec![ls("a", 0.0)], vec![ls("b", 0.0)], vec![ls("c", 0.0)],]
        );
    }

    #[test]
    fn test_many_spans_same_line() {
        let spans: Vec<_> = (0..10)
            .map(|i| span(&format!("s{}", i), i as f32 * 0.1))
            .collect();
        let result = split_spans_into_lines(spans);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].len(), 10);
    }
}
