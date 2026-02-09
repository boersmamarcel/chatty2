//! Math expression parser for extracting LaTeX from markdown content
//!
//! Supports:
//! - Inline math: $...$, \(...\)
//! - Block math: $$...$$ (on own lines), ```math\n...\n```, ```latex\n...\n```, \[...\]
//! - LaTeX environments: \begin{equation}, \begin{align}, \begin{cases}, etc.
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

/// Common LaTeX environments that should be treated as block math
const LATEX_ENVIRONMENTS: &[&str] = &[
    // Equation environments
    "equation",
    "equation*",
    "align",
    "align*",
    "gather",
    "gather*",
    // Multi-line environments
    "multline",
    "multline*",
    "split",
    "alignat",
    "alignat*",
    // Matrix/cases environments
    "matrix",
    "pmatrix",
    "bmatrix",
    "Bmatrix",
    "vmatrix",
    "Vmatrix",
    "cases",
    // Nested/alignment environments
    "aligned",
    "gathered",
    "alignedat",
];

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

/// Helper function to parse math code blocks (```math or ```latex)
fn parse_math_code_block(
    chars: &[char],
    start_pos: usize,
    language: &str,
) -> Option<(String, usize)> {
    let lang_chars: Vec<char> = language.chars().collect();
    let lang_len = lang_chars.len();

    // Check if it matches the expected language
    if start_pos + 3 + lang_len >= chars.len() {
        return None;
    }

    if chars[start_pos..start_pos + 3] != ['`', '`', '`'] {
        return None;
    }

    if chars[start_pos + 3..start_pos + 3 + lang_len] != lang_chars[..] {
        return None;
    }

    // Check if it's actually ```<lang> (not ```<lang>something or similar)
    if start_pos + 3 + lang_len < chars.len()
        && !(chars[start_pos + 3 + lang_len] == '\n' || chars[start_pos + 3 + lang_len] == '\r')
    {
        return None;
    }

    // Find the closing ```
    let mut i = start_pos + 3 + lang_len;
    while i < chars.len() && (chars[i] == '\n' || chars[i] == '\r') {
        i += 1; // Skip newlines after ```<lang>
    }

    let math_start = i;
    while i + 2 < chars.len() {
        if chars[i] == '`' && chars[i + 1] == '`' && chars[i + 2] == '`' {
            // Found closing ```
            let math_content: String = chars[math_start..i].iter().collect();
            i += 3; // Skip closing ```

            if !math_content.trim().is_empty() {
                return Some((math_content.trim().to_string(), i));
            }
            return None;
        }
        i += 1;
    }

    // No closing ``` found
    None
}

/// Helper function to extract environment name from \begin{...}
fn extract_environment_name(chars: &[char], start_pos: usize) -> Option<(String, usize)> {
    // start_pos should point to the '{' after \begin
    let mut i = start_pos;
    if i >= chars.len() || chars[i] != '{' {
        return None;
    }

    i += 1; // Skip '{'
    let name_start = i;

    // Find closing '}'
    while i < chars.len() && chars[i] != '}' {
        i += 1;
    }

    if i >= chars.len() {
        return None;
    }

    let name: String = chars[name_start..i].iter().collect();
    i += 1; // Skip '}'

    Some((name, i))
}

/// Parse LaTeX environment: \begin{env}...\end{env}
fn parse_latex_environment(chars: &[char], start_pos: usize) -> Option<(String, usize)> {
    // start_pos should point to '\' in \begin
    if start_pos + 7 >= chars.len() {
        return None;
    }

    // Check for \begin{
    if chars[start_pos..start_pos + 7] != ['\\', 'b', 'e', 'g', 'i', 'n', '{'] {
        return None;
    }

    // Extract environment name
    let (env_name, pos_after_name) = extract_environment_name(chars, start_pos + 6)?;

    // Check if it's a recognized environment
    if !LATEX_ENVIRONMENTS.contains(&env_name.as_str()) {
        return None;
    }

    // Find matching \end{env_name}
    let end_pattern = format!("\\end{{{}}}", env_name);
    let end_chars: Vec<char> = end_pattern.chars().collect();

    let mut i = pos_after_name;
    let mut depth = 1; // Track nesting depth

    while i < chars.len() {
        // Check for nested \begin{env_name}
        if i + 7 + env_name.len() < chars.len() {
            let potential_begin = format!("\\begin{{{}}}", env_name);
            let begin_chars: Vec<char> = potential_begin.chars().collect();
            if i + begin_chars.len() <= chars.len()
                && chars[i..i + begin_chars.len()] == begin_chars[..]
            {
                depth += 1;
                i += begin_chars.len();
                continue;
            }
        }

        // Check for \end{env_name}
        if i + end_chars.len() <= chars.len() && chars[i..i + end_chars.len()] == end_chars[..] {
            depth -= 1;
            if depth == 0 {
                // Found matching \end
                let content: String = chars[start_pos..i + end_chars.len()].iter().collect();
                return Some((content, i + end_chars.len()));
            }
            i += end_chars.len();
            continue;
        }

        i += 1;
    }

    None
}

/// Parse content into segments of text and math expressions
pub fn parse_math_segments(content: &str) -> Vec<MathSegment> {
    let mut segments = Vec::new();
    let chars: Vec<char> = content.chars().collect();
    let mut i = 0;
    let mut current_text = String::new();

    while i < chars.len() {
        // Check for LaTeX environments: \begin{...}
        if i + 7 < chars.len()
            && chars[i] == '\\'
            && chars[i + 1..i + 6] == ['b', 'e', 'g', 'i', 'n']
            && chars[i + 6] == '{'
            && let Some((env_content, new_pos)) = parse_latex_environment(&chars, i)
        {
            // Save any accumulated text
            if !current_text.is_empty() {
                segments.push(MathSegment::Text(current_text.clone()));
                current_text.clear();
            }

            segments.push(MathSegment::BlockMath(env_content));
            i = new_pos;
            continue;
        }

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

        // Check for block math: ```math or ```latex
        if i + 7 < chars.len() && chars[i] == '`' && chars[i + 1] == '`' && chars[i + 2] == '`' {
            // Try ```math first
            if let Some((math_content, new_pos)) = parse_math_code_block(&chars, i, "math") {
                // Save any accumulated text
                if !current_text.is_empty() {
                    segments.push(MathSegment::Text(current_text.clone()));
                    current_text.clear();
                }

                segments.push(MathSegment::BlockMath(math_content));
                i = new_pos;
                continue;
            }

            // Try ```latex
            if let Some((math_content, new_pos)) = parse_math_code_block(&chars, i, "latex") {
                // Save any accumulated text
                if !current_text.is_empty() {
                    segments.push(MathSegment::Text(current_text.clone()));
                    current_text.clear();
                }

                segments.push(MathSegment::BlockMath(math_content));
                i = new_pos;
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
            let mut has_newlines = false;

            while j + 1 < chars.len() {
                if chars[j] == '\n' || chars[j] == '\r' {
                    has_newlines = true;
                }

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

                        // Decide if it's block or inline based on position and content
                        // If content has newlines, bias towards block math
                        if (is_block_start && is_block_end) || has_newlines {
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

    #[test]
    fn test_latex_code_block() {
        let input = "Here's an equation:\n```latex\nx = y + 2\n```\nDone.";
        let segments = parse_math_segments(input);
        assert_eq!(segments.len(), 3);
        assert!(matches!(segments[1], MathSegment::BlockMath(_)));
        if let MathSegment::BlockMath(content) = &segments[1] {
            assert_eq!(content, "x = y + 2");
        }
    }

    #[test]
    fn test_equation_environment() {
        let input = "\\begin{equation}\nx^2 + y^2 = z^2\n\\end{equation}";
        let segments = parse_math_segments(input);
        assert_eq!(segments.len(), 1);
        assert!(matches!(segments[0], MathSegment::BlockMath(_)));
    }

    #[test]
    fn test_align_environment() {
        let input = "\\begin{align}\na &= b + c \\\\\nd &= e + f\n\\end{align}";
        let segments = parse_math_segments(input);
        assert_eq!(segments.len(), 1);
        assert!(matches!(segments[0], MathSegment::BlockMath(_)));
    }

    #[test]
    fn test_nested_environments() {
        let input = "\\begin{equation}\nx = \\begin{cases}\n1 & \\text{if } x > 0 \\\\\n0 & \\text{otherwise}\n\\end{cases}\n\\end{equation}";
        let segments = parse_math_segments(input);
        assert_eq!(segments.len(), 1);
        assert!(matches!(segments[0], MathSegment::BlockMath(_)));
    }

    #[test]
    fn test_block_math_with_intro_text() {
        let input = "Consider: $$\nx^2 + y^2 = z^2\n$$";
        let segments = parse_math_segments(input);
        // Should recognize as block math despite intro text (has newlines)
        assert!(
            segments
                .iter()
                .any(|s| matches!(s, MathSegment::BlockMath(_)))
        );
    }

    #[test]
    fn test_mixed_delimiters() {
        let input = "Inline $x = 2$ and block:\n$$\ny = mx + b\n$$\nAlso \\[z = a + b\\]";
        let segments = parse_math_segments(input);
        assert_eq!(
            segments
                .iter()
                .filter(|s| matches!(s, MathSegment::InlineMath(_)))
                .count(),
            1
        );
        assert_eq!(
            segments
                .iter()
                .filter(|s| matches!(s, MathSegment::BlockMath(_)))
                .count(),
            2
        );
    }

    #[test]
    fn test_mismatched_environment_tags() {
        let input = "\\begin{align}\nx = 2\n\\end{equation}";
        let segments = parse_math_segments(input);
        // Should NOT recognize as math due to mismatch
        assert!(segments.iter().all(|s| matches!(s, MathSegment::Text(_))));
    }

    #[test]
    fn test_existing_patterns_preserved() {
        // Test that all existing delimiter patterns work correctly
        let input = "Inline $x$ and \\(y\\) and block\n$$z$$\nand \\[w\\] and ```math\na\n```";
        let segments = parse_math_segments(input);

        let inline_count = segments
            .iter()
            .filter(|s| matches!(s, MathSegment::InlineMath(_)))
            .count();
        let block_count = segments
            .iter()
            .filter(|s| matches!(s, MathSegment::BlockMath(_)))
            .count();

        // Inline: $x$ and \(y\) = 2
        // Block: $$z$$ (has newlines), \[w\], ```math = 3
        assert_eq!(inline_count, 2);
        assert_eq!(block_count, 3);
    }

    #[test]
    fn test_math_and_latex_code_blocks() {
        let input = "```math\nx = 1\n```\nand\n```latex\ny = 2\n```";
        let segments = parse_math_segments(input);
        assert_eq!(
            segments
                .iter()
                .filter(|s| matches!(s, MathSegment::BlockMath(_)))
                .count(),
            2
        );
    }

    #[test]
    fn test_matrix_environment() {
        let input = "\\begin{pmatrix}\n1 & 2 \\\\\n3 & 4\n\\end{pmatrix}";
        let segments = parse_math_segments(input);
        assert_eq!(segments.len(), 1);
        assert!(matches!(segments[0], MathSegment::BlockMath(_)));
    }
}
