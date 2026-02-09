//! Math expression parser for extracting LaTeX from markdown content
//!
//! Supports:
//! - Inline math: $...$, \(...\)
//! - Block math: $$...$$ (on own lines), ```math\n...\n```, \[...\]
//! - Escaped dollars: \$ (not treated as math)

/// Represents a segment of content that may contain math or text
#[derive(Clone, Debug, PartialEq)]
pub enum MathSegment {
    /// Regular text content (may contain markdown)
    Text(String),
    /// Inline math expression (content between $ or $$)
    InlineMath(String),
    /// Block/display math expression (content in ```math blocks)
    BlockMath(String),
}

/// Check if $$ at position i should be treated as block math start
fn is_block_math_delimiter(chars: &[char], pos: usize) -> bool {
    // Check if there's only whitespace/newlines before this on the current line
    let mut i = pos;
    while i > 0 {
        i -= 1;
        if chars[i] == '\n' || chars[i] == '\r' {
            // Found newline, so $$ is at start of line (possibly with whitespace)
            return true;
        }
        if !chars[i].is_whitespace() {
            // Found non-whitespace, so this is inline
            return false;
        }
    }
    // Reached start of string
    true
}

/// Parse content into segments of text and math expressions
pub fn parse_math_segments(content: &str) -> Vec<MathSegment> {
    let mut segments = Vec::new();
    let chars: Vec<char> = content.chars().collect();
    let mut i = 0;
    let mut current_text = String::new();

    while i < chars.len() {
        // Check for LaTeX display math: \[
        if i + 1 < chars.len() && chars[i] == '\\' && chars[i + 1] == '[' {
            // Save any accumulated text
            if !current_text.is_empty() {
                segments.push(MathSegment::Text(current_text.clone()));
                current_text.clear();
            }

            // Find closing \]
            i += 2; // Skip \[
            let math_start = i;
            let mut found_close = false;

            while i + 1 < chars.len() {
                if chars[i] == '\\' && chars[i + 1] == ']' {
                    let math_content: String = chars[math_start..i].iter().collect();
                    found_close = true;
                    i += 2; // Skip \]

                    if !math_content.trim().is_empty() {
                        segments.push(MathSegment::BlockMath(math_content.trim().to_string()));
                    }
                    break;
                }
                i += 1;
            }

            if !found_close {
                // Unclosed \[ - treat as literal
                current_text.push_str("\\[");
                current_text.push_str(&chars[math_start..i].iter().collect::<String>());
            }

            continue;
        }

        // Check for LaTeX inline math: \(
        if i + 1 < chars.len() && chars[i] == '\\' && chars[i + 1] == '(' {
            // Find closing \)
            i += 2; // Skip \(
            let math_start = i;
            let mut found_close = false;

            while i + 1 < chars.len() {
                if chars[i] == '\\' && chars[i + 1] == ')' {
                    let math_content: String = chars[math_start..i].iter().collect();
                    found_close = true;
                    i += 2; // Skip \)

                    if !math_content.trim().is_empty() {
                        // Save any accumulated text
                        if !current_text.is_empty() {
                            segments.push(MathSegment::Text(current_text.clone()));
                            current_text.clear();
                        }
                        segments.push(MathSegment::InlineMath(math_content.trim().to_string()));
                    }
                    break;
                }
                i += 1;
            }

            if !found_close {
                // Unclosed \( - treat as literal
                current_text.push_str("\\(");
                current_text.push_str(&chars[math_start..i].iter().collect::<String>());
            }

            continue;
        }

        // Check for block math: ```math
        if i + 7 < chars.len()
            && chars[i] == '`'
            && chars[i + 1] == '`'
            && chars[i + 2] == '`'
            && chars[i + 3..i + 7] == ['m', 'a', 't', 'h']
        {
            // Check if it's actually ```math (not ```mathematics or similar)
            if i + 7 < chars.len() && (chars[i + 7] == '\n' || chars[i + 7] == '\r') {
                // Save any accumulated text
                if !current_text.is_empty() {
                    segments.push(MathSegment::Text(current_text.clone()));
                    current_text.clear();
                }

                // Find the closing ```
                i += 7; // Skip ```math
                while i < chars.len() && (chars[i] == '\n' || chars[i] == '\r') {
                    i += 1; // Skip newlines after ```math
                }

                let math_start = i;
                let mut math_content = String::new();
                let mut found_close = false;

                while i + 2 < chars.len() {
                    if chars[i] == '`' && chars[i + 1] == '`' && chars[i + 2] == '`' {
                        // Found closing ```
                        math_content = chars[math_start..i].iter().collect();
                        found_close = true;
                        i += 3; // Skip closing ```
                        break;
                    }
                    i += 1;
                }

                if found_close && !math_content.trim().is_empty() {
                    segments.push(MathSegment::BlockMath(math_content.trim().to_string()));
                } else if !found_close {
                    // Unclosed block math - treat as literal text
                    current_text.push_str("```math");
                    current_text.push_str(&chars[math_start..i].iter().collect::<String>());
                }

                continue;
            }
        }

        // Check for $$ (could be block or inline depending on context)
        if i + 1 < chars.len()
            && chars[i] == '$'
            && chars[i + 1] == '$'
            && (i == 0 || chars[i - 1] != '\\')
        {
            // Check if $$ is on its own line (block math)
            let is_block_start = is_block_math_delimiter(&chars, i);

            // Look for closing $$
            let mut j = i + 2;
            let mut found_close = false;

            while j + 1 < chars.len() {
                if chars[j] == '$' && chars[j + 1] == '$' {
                    // Check for escaped $$
                    if j > 0 && chars[j - 1] == '\\' {
                        j += 2;
                        continue;
                    }

                    let math_content: String = chars[i + 2..j].iter().collect();
                    found_close = true;

                    // Check if closing $$ is followed by newline or end (confirms block)
                    let mut is_block_end = false;
                    let mut end_pos = j + 2;
                    while end_pos < chars.len() {
                        if chars[end_pos] == '\n' || chars[end_pos] == '\r' {
                            is_block_end = true;
                            break;
                        }
                        if !chars[end_pos].is_whitespace() {
                            break;
                        }
                        end_pos += 1;
                    }
                    if end_pos >= chars.len() {
                        is_block_end = true;
                    }

                    if !math_content.trim().is_empty() {
                        // Save any accumulated text
                        if !current_text.is_empty() {
                            segments.push(MathSegment::Text(current_text.clone()));
                            current_text.clear();
                        }

                        // Decide if it's block or inline based on position
                        if is_block_start && is_block_end {
                            segments.push(MathSegment::BlockMath(math_content.trim().to_string()));
                        } else {
                            segments.push(MathSegment::InlineMath(math_content.trim().to_string()));
                        }
                    }

                    i = j + 2; // Skip closing $$
                    break;
                }
                j += 1;
            }

            if !found_close {
                // No closing $$ found - treat as literal text
                current_text.push('$');
                i += 1;
                continue;
            }

            continue;
        }

        // Check for single $ (inline math)
        if chars[i] == '$' && (i == 0 || chars[i - 1] != '\\') {
            // Look for closing $
            let mut j = i + 1;
            let mut found_close = false;

            while j < chars.len() {
                if chars[j] == '$' {
                    // Check for escaped $
                    if j > 0 && chars[j - 1] == '\\' {
                        j += 1;
                        continue;
                    }

                    // Found closing $
                    found_close = true;
                    let math_content: String = chars[i + 1..j].iter().collect();

                    if !math_content.trim().is_empty() {
                        // Save any accumulated text
                        if !current_text.is_empty() {
                            segments.push(MathSegment::Text(current_text.clone()));
                            current_text.clear();
                        }

                        segments.push(MathSegment::InlineMath(math_content.trim().to_string()));
                    }

                    i = j + 1; // Skip closing $
                    break;
                }
                j += 1;
            }

            if !found_close {
                // No closing delimiter found - treat as literal text
                current_text.push(chars[i]);
                i += 1;
            }

            continue;
        }

        // Regular character - add to current text
        current_text.push(chars[i]);
        i += 1;
    }

    // Add any remaining text
    if !current_text.is_empty() {
        segments.push(MathSegment::Text(current_text));
    }

    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==========================================
    // Plain text (no math)
    // ==========================================

    #[test]
    fn test_plain_text() {
        let result = parse_math_segments("Hello, world!");
        assert_eq!(result, vec![MathSegment::Text("Hello, world!".into())]);
    }

    #[test]
    fn test_empty_string() {
        let result = parse_math_segments("");
        assert_eq!(result, vec![]);
    }

    #[test]
    fn test_whitespace_only() {
        let result = parse_math_segments("   \n\t  ");
        assert_eq!(result, vec![MathSegment::Text("   \n\t  ".into())]);
    }

    // ==========================================
    // Inline math: $...$
    // ==========================================

    #[test]
    fn test_inline_math_basic() {
        let result = parse_math_segments("The formula $x^2$ is simple.");
        assert_eq!(
            result,
            vec![
                MathSegment::Text("The formula ".into()),
                MathSegment::InlineMath("x^2".into()),
                MathSegment::Text(" is simple.".into()),
            ]
        );
    }

    #[test]
    fn test_inline_math_at_start() {
        let result = parse_math_segments("$x$ is a variable");
        assert_eq!(
            result,
            vec![
                MathSegment::InlineMath("x".into()),
                MathSegment::Text(" is a variable".into()),
            ]
        );
    }

    #[test]
    fn test_inline_math_at_end() {
        let result = parse_math_segments("The answer is $42$");
        assert_eq!(
            result,
            vec![
                MathSegment::Text("The answer is ".into()),
                MathSegment::InlineMath("42".into()),
            ]
        );
    }

    #[test]
    fn test_multiple_inline_math() {
        let result = parse_math_segments("$a$ and $b$ and $c$");
        assert_eq!(
            result,
            vec![
                MathSegment::InlineMath("a".into()),
                MathSegment::Text(" and ".into()),
                MathSegment::InlineMath("b".into()),
                MathSegment::Text(" and ".into()),
                MathSegment::InlineMath("c".into()),
            ]
        );
    }

    #[test]
    fn test_inline_math_with_spaces() {
        let result = parse_math_segments("$ x + y $");
        assert_eq!(result, vec![MathSegment::InlineMath("x + y".into())]);
    }

    #[test]
    fn test_inline_math_complex_expression() {
        let result = parse_math_segments("Given $\\frac{a}{b} + \\sqrt{c}$ we get");
        assert_eq!(
            result,
            vec![
                MathSegment::Text("Given ".into()),
                MathSegment::InlineMath("\\frac{a}{b} + \\sqrt{c}".into()),
                MathSegment::Text(" we get".into()),
            ]
        );
    }

    #[test]
    fn test_empty_inline_math_ignored() {
        // Empty math (only whitespace) should not produce a math segment
        let result = parse_math_segments("before $ $ after");
        assert_eq!(result, vec![MathSegment::Text("before  after".into())]);
    }

    // ==========================================
    // Escaped dollar signs: \$
    // ==========================================

    #[test]
    fn test_escaped_dollar_not_math() {
        let result = parse_math_segments("Price is \\$5 each");
        assert_eq!(result, vec![MathSegment::Text("Price is \\$5 each".into())]);
    }

    #[test]
    fn test_escaped_dollar_before_math() {
        let result = parse_math_segments("\\$100 and $x$");
        assert_eq!(
            result,
            vec![
                MathSegment::Text("\\$100 and ".into()),
                MathSegment::InlineMath("x".into()),
            ]
        );
    }

    #[test]
    fn test_escaped_dollar_inside_math() {
        // escaped $ inside $...$ should not close the math
        let result = parse_math_segments("$a \\$ b$");
        assert_eq!(result, vec![MathSegment::InlineMath("a \\$ b".into())]);
    }

    // ==========================================
    // Block math: $$...$$
    // ==========================================

    #[test]
    fn test_block_math_on_own_lines() {
        let result = parse_math_segments("before\n$$\nx^2 + y^2 = z^2\n$$\nafter");
        assert_eq!(
            result,
            vec![
                MathSegment::Text("before\n".into()),
                MathSegment::BlockMath("x^2 + y^2 = z^2".into()),
                MathSegment::Text("\nafter".into()),
            ]
        );
    }

    #[test]
    fn test_block_math_at_string_start() {
        let result = parse_math_segments("$$\nE = mc^2\n$$\ntext");
        assert_eq!(
            result,
            vec![
                MathSegment::BlockMath("E = mc^2".into()),
                MathSegment::Text("\ntext".into()),
            ]
        );
    }

    #[test]
    fn test_block_math_at_string_end() {
        let result = parse_math_segments("text\n$$\nE = mc^2\n$$");
        assert_eq!(
            result,
            vec![
                MathSegment::Text("text\n".into()),
                MathSegment::BlockMath("E = mc^2".into()),
            ]
        );
    }

    #[test]
    fn test_double_dollar_inline_when_not_on_own_line() {
        // $$ used inline (not at start/end of line) should be InlineMath
        let result = parse_math_segments("The formula $$x^2$$ is inline");
        assert_eq!(
            result,
            vec![
                MathSegment::Text("The formula ".into()),
                MathSegment::InlineMath("x^2".into()),
                MathSegment::Text(" is inline".into()),
            ]
        );
    }

    #[test]
    fn test_block_math_multiline_content() {
        let input = "$$\n\\begin{align}\na &= b \\\\\nc &= d\n\\end{align}\n$$";
        let result = parse_math_segments(input);
        assert_eq!(
            result,
            vec![MathSegment::BlockMath(
                "\\begin{align}\na &= b \\\\\nc &= d\n\\end{align}".into()
            )]
        );
    }

    // ==========================================
    // Fenced code block math: ```math
    // ==========================================

    #[test]
    fn test_fenced_math_block() {
        let input = "```math\nx^2 + y^2\n```";
        let result = parse_math_segments(input);
        assert_eq!(result, vec![MathSegment::BlockMath("x^2 + y^2".into())]);
    }

    #[test]
    fn test_fenced_math_block_with_surrounding_text() {
        let input = "Before\n```math\na + b\n```\nAfter";
        let result = parse_math_segments(input);
        assert_eq!(
            result,
            vec![
                MathSegment::Text("Before\n".into()),
                MathSegment::BlockMath("a + b".into()),
                MathSegment::Text("\nAfter".into()),
            ]
        );
    }

    #[test]
    fn test_fenced_math_multiline() {
        let input = "```math\nline1\nline2\nline3\n```";
        let result = parse_math_segments(input);
        assert_eq!(
            result,
            vec![MathSegment::BlockMath("line1\nline2\nline3".into())]
        );
    }

    #[test]
    fn test_unclosed_fenced_math_block() {
        // Unclosed ```math should be treated as literal text
        let input = "```math\nx^2 + y^2";
        let result = parse_math_segments(input);
        assert_eq!(result, vec![MathSegment::Text("```mathx^2 + y^2".into())]);
    }

    #[test]
    fn test_fenced_math_not_mathematics() {
        // ```mathematics should not be treated as math block (no newline at pos i+7)
        let input = "```mathematics\ncontent\n```";
        let result = parse_math_segments(input);
        // "mathematics" doesn't have \n at position 7 relative to ```, so not parsed as math block
        assert!(
            !result
                .iter()
                .any(|s| matches!(s, MathSegment::BlockMath(_))),
            "```mathematics should not produce a BlockMath segment"
        );
    }

    // ==========================================
    // LaTeX display math: \[...\]
    // ==========================================

    #[test]
    fn test_latex_display_math() {
        let result = parse_math_segments("\\[x^2 + y^2 = z^2\\]");
        assert_eq!(
            result,
            vec![MathSegment::BlockMath("x^2 + y^2 = z^2".into())]
        );
    }

    #[test]
    fn test_latex_display_math_with_surrounding_text() {
        let result = parse_math_segments("Before \\[a + b\\] after");
        assert_eq!(
            result,
            vec![
                MathSegment::Text("Before ".into()),
                MathSegment::BlockMath("a + b".into()),
                MathSegment::Text(" after".into()),
            ]
        );
    }

    #[test]
    fn test_unclosed_latex_display_math() {
        let result = parse_math_segments("\\[x^2 + y^2");
        assert_eq!(result, vec![MathSegment::Text("\\[x^2 + y^2".into())]);
    }

    #[test]
    fn test_latex_display_math_empty_content() {
        // Empty \[\] should not produce a BlockMath segment
        let result = parse_math_segments("\\[  \\]");
        assert!(
            !result
                .iter()
                .any(|s| matches!(s, MathSegment::BlockMath(_))),
            "Empty \\[\\] should not produce BlockMath"
        );
    }

    // ==========================================
    // LaTeX inline math: \(...\)
    // ==========================================

    #[test]
    fn test_latex_inline_math() {
        let result = parse_math_segments("The formula \\(x^2\\) is simple.");
        assert_eq!(
            result,
            vec![
                MathSegment::Text("The formula ".into()),
                MathSegment::InlineMath("x^2".into()),
                MathSegment::Text(" is simple.".into()),
            ]
        );
    }

    #[test]
    fn test_unclosed_latex_inline_math() {
        let result = parse_math_segments("\\(x^2 forever");
        assert_eq!(result, vec![MathSegment::Text("\\(x^2 forever".into())]);
    }

    #[test]
    fn test_latex_inline_math_empty_content() {
        // Empty \(\) should not produce InlineMath
        let result = parse_math_segments("before \\(  \\) after");
        assert!(
            !result
                .iter()
                .any(|s| matches!(s, MathSegment::InlineMath(_))),
            "Empty \\(\\) should not produce InlineMath"
        );
    }

    #[test]
    fn test_multiple_latex_inline() {
        let result = parse_math_segments("\\(a\\) and \\(b\\)");
        assert_eq!(
            result,
            vec![
                MathSegment::InlineMath("a".into()),
                MathSegment::Text(" and ".into()),
                MathSegment::InlineMath("b".into()),
            ]
        );
    }

    // ==========================================
    // Mixed delimiter types
    // ==========================================

    #[test]
    fn test_mixed_inline_dollar_and_latex() {
        let result = parse_math_segments("$a$ and \\(b\\) together");
        assert_eq!(
            result,
            vec![
                MathSegment::InlineMath("a".into()),
                MathSegment::Text(" and ".into()),
                MathSegment::InlineMath("b".into()),
                MathSegment::Text(" together".into()),
            ]
        );
    }

    #[test]
    fn test_mixed_block_and_inline() {
        let input = "Inline $x$ then block\n$$\ny = mx + b\n$$\nmore text";
        let result = parse_math_segments(input);
        assert_eq!(
            result,
            vec![
                MathSegment::Text("Inline ".into()),
                MathSegment::InlineMath("x".into()),
                MathSegment::Text(" then block\n".into()),
                MathSegment::BlockMath("y = mx + b".into()),
                MathSegment::Text("\nmore text".into()),
            ]
        );
    }

    // ==========================================
    // Unclosed delimiters
    // ==========================================

    #[test]
    fn test_unclosed_single_dollar() {
        let result = parse_math_segments("Price is $5 forever");
        // No closing $ that isn't preceded by text, so treat as literal
        // Actually the parser will try to find closing $: "5 forever" is the content
        // Wait - let me re-read: it looks for any closing $. "5 forever" has no closing $.
        // But wait, there's no second $ at all, so it's unclosed -> literal
        assert_eq!(
            result,
            vec![MathSegment::Text("Price is $5 forever".into())]
        );
    }

    #[test]
    fn test_unclosed_double_dollar() {
        let result = parse_math_segments("$$unclosed math expression");
        // Only one $ consumed as literal, then "unclosed..." is text
        assert_eq!(
            result,
            vec![MathSegment::Text("$$unclosed math expression".into())]
        );
    }

    // ==========================================
    // is_block_math_delimiter tests
    // ==========================================

    #[test]
    fn test_block_delimiter_at_string_start() {
        let chars: Vec<char> = "$$content$$".chars().collect();
        assert!(is_block_math_delimiter(&chars, 0));
    }

    #[test]
    fn test_block_delimiter_after_newline() {
        let chars: Vec<char> = "text\n$$content$$".chars().collect();
        assert!(is_block_math_delimiter(&chars, 5));
    }

    #[test]
    fn test_block_delimiter_after_text_on_same_line() {
        let chars: Vec<char> = "text $$content$$".chars().collect();
        assert!(!is_block_math_delimiter(&chars, 5));
    }

    #[test]
    fn test_block_delimiter_after_whitespace_and_newline() {
        let chars: Vec<char> = "text\n  $$content$$".chars().collect();
        assert!(is_block_math_delimiter(&chars, 7));
    }

    // ==========================================
    // Edge cases
    // ==========================================

    #[test]
    fn test_consecutive_dollar_signs() {
        // Single dollar that finds another dollar immediately: $$ which may be treated as block
        let result = parse_math_segments("$$$$");
        // First $$ opens, second $$ closes -> empty content -> ignored
        assert_eq!(result, vec![]);
    }

    #[test]
    fn test_single_dollar_sign_alone() {
        let result = parse_math_segments("$");
        assert_eq!(result, vec![MathSegment::Text("$".into())]);
    }

    #[test]
    fn test_two_dollar_signs_alone() {
        let result = parse_math_segments("$$");
        // $$ opens but no closing $$ found -> first $ treated as literal, second $ unclosed
        assert_eq!(result, vec![MathSegment::Text("$$".into())]);
    }

    #[test]
    fn test_math_with_newlines_in_inline() {
        let result = parse_math_segments("$a\nb$");
        assert_eq!(result, vec![MathSegment::InlineMath("a\nb".into())]);
    }

    #[test]
    fn test_backslash_not_followed_by_bracket() {
        // Backslash followed by something other than [ or ( should be regular text
        let result = parse_math_segments("\\n text \\t more");
        assert_eq!(result, vec![MathSegment::Text("\\n text \\t more".into())]);
    }

    #[test]
    fn test_only_math_no_text() {
        let result = parse_math_segments("$x$");
        assert_eq!(result, vec![MathSegment::InlineMath("x".into())]);
    }

    #[test]
    fn test_adjacent_math_expressions() {
        let result = parse_math_segments("$a$$b$");
        // First $a$ is parsed, then $b$ is parsed
        assert_eq!(
            result,
            vec![
                MathSegment::InlineMath("a".into()),
                MathSegment::InlineMath("b".into()),
            ]
        );
    }

    #[test]
    fn test_escaped_dollar_in_double_dollar() {
        let result = parse_math_segments("$$a \\$$ b$$");
        // $$ at string start -> is_block_math_delimiter returns true
        // The \$$ inside has an escaped $, so the parser skips it, finding the real closing $$
        // Closing $$ at end of string -> is_block_end = true
        // Both block start and block end -> BlockMath
        assert_eq!(result, vec![MathSegment::BlockMath("a \\$$ b".into())]);
    }

    #[test]
    fn test_real_world_quadratic_formula() {
        let input = "The quadratic formula is $x = \\frac{-b \\pm \\sqrt{b^2 - 4ac}}{2a}$ where $a \\neq 0$.";
        let result = parse_math_segments(input);
        assert_eq!(
            result,
            vec![
                MathSegment::Text("The quadratic formula is ".into()),
                MathSegment::InlineMath("x = \\frac{-b \\pm \\sqrt{b^2 - 4ac}}{2a}".into()),
                MathSegment::Text(" where ".into()),
                MathSegment::InlineMath("a \\neq 0".into()),
                MathSegment::Text(".".into()),
            ]
        );
    }

    #[test]
    fn test_real_world_block_equation() {
        let input = "Consider:\n$$\n\\int_0^\\infty e^{-x^2} dx = \\frac{\\sqrt{\\pi}}{2}\n$$\nThis is the Gaussian integral.";
        let result = parse_math_segments(input);
        assert_eq!(
            result,
            vec![
                MathSegment::Text("Consider:\n".into()),
                MathSegment::BlockMath(
                    "\\int_0^\\infty e^{-x^2} dx = \\frac{\\sqrt{\\pi}}{2}".into()
                ),
                MathSegment::Text("\nThis is the Gaussian integral.".into()),
            ]
        );
    }

    #[test]
    fn test_latex_display_with_multiline() {
        let input = "\\[\n\\sum_{i=1}^{n} i = \\frac{n(n+1)}{2}\n\\]";
        let result = parse_math_segments(input);
        assert_eq!(
            result,
            vec![MathSegment::BlockMath(
                "\\sum_{i=1}^{n} i = \\frac{n(n+1)}{2}".into()
            )]
        );
    }

    #[test]
    fn test_unicode_in_math() {
        let result = parse_math_segments("$α + β = γ$");
        assert_eq!(result, vec![MathSegment::InlineMath("α + β = γ".into())]);
    }

    #[test]
    fn test_long_text_with_scattered_math() {
        let input = "In physics, $F = ma$ describes force. \
                     The energy equation \\(E = mc^2\\) is famous. \
                     For waves:\n$$\nv = f\\lambda\n$$\nThat's it.";
        let result = parse_math_segments(input);
        assert_eq!(
            result,
            vec![
                MathSegment::Text("In physics, ".into()),
                MathSegment::InlineMath("F = ma".into()),
                MathSegment::Text(" describes force. The energy equation ".into()),
                MathSegment::InlineMath("E = mc^2".into()),
                MathSegment::Text(" is famous. For waves:\n".into()),
                MathSegment::BlockMath("v = f\\lambda".into()),
                MathSegment::Text("\nThat's it.".into()),
            ]
        );
    }
}
