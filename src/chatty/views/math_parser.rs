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

/// Internal parser struct that provides efficient char-indexed access
/// with zero-copy string extraction via byte offsets.
struct MathParser<'a> {
    content: &'a str,
    chars: Vec<char>,
    /// byte_offsets[i] = byte index of chars[i]; byte_offsets[chars.len()] = content.len()
    byte_offsets: Vec<usize>,
}

impl<'a> MathParser<'a> {
    fn new(content: &'a str) -> Self {
        let chars: Vec<char> = content.chars().collect();
        let byte_offsets: Vec<usize> = content
            .char_indices()
            .map(|(idx, _)| idx)
            .chain(std::iter::once(content.len()))
            .collect();
        Self {
            content,
            chars,
            byte_offsets,
        }
    }

    /// Get a &str slice from char index `start` to char index `end`.
    fn slice(&self, start: usize, end: usize) -> &str {
        &self.content[self.byte_offsets[start]..self.byte_offsets[end]]
    }

    /// Check if $$ at position `pos` should be treated as block math start
    fn is_block_math_delimiter(&self, pos: usize) -> bool {
        let mut i = pos;
        while i > 0 {
            i -= 1;
            if self.chars[i] == '\n' || self.chars[i] == '\r' {
                return true;
            }
            if !self.chars[i].is_whitespace() {
                return false;
            }
        }
        true
    }

    /// Parse math code blocks (```math or ```latex)
    fn parse_math_code_block(&self, start_pos: usize, language: &str) -> Option<(String, usize)> {
        let lang_chars: Vec<char> = language.chars().collect();
        let lang_len = lang_chars.len();

        if start_pos + 3 + lang_len >= self.chars.len() {
            return None;
        }

        if self.chars[start_pos..start_pos + 3] != ['`', '`', '`'] {
            return None;
        }

        if self.chars[start_pos + 3..start_pos + 3 + lang_len] != lang_chars[..] {
            return None;
        }

        if start_pos + 3 + lang_len < self.chars.len()
            && !(self.chars[start_pos + 3 + lang_len] == '\n'
                || self.chars[start_pos + 3 + lang_len] == '\r')
        {
            return None;
        }

        let mut i = start_pos + 3 + lang_len;
        while i < self.chars.len() && (self.chars[i] == '\n' || self.chars[i] == '\r') {
            i += 1;
        }

        let math_start = i;
        while i + 2 < self.chars.len() {
            if self.chars[i] == '`' && self.chars[i + 1] == '`' && self.chars[i + 2] == '`' {
                let math_content = self.slice(math_start, i);
                i += 3;

                if !math_content.trim().is_empty() {
                    return Some((math_content.trim().to_string(), i));
                }
                return None;
            }
            i += 1;
        }

        None
    }

    /// Extract environment name from \begin{...}
    fn extract_environment_name(&self, start_pos: usize) -> Option<(String, usize)> {
        let mut i = start_pos;
        if i >= self.chars.len() || self.chars[i] != '{' {
            return None;
        }

        i += 1;
        let name_start = i;

        while i < self.chars.len() && self.chars[i] != '}' {
            i += 1;
        }

        if i >= self.chars.len() {
            return None;
        }

        let name = self.slice(name_start, i).to_string();
        i += 1;

        Some((name, i))
    }

    /// Parse LaTeX environment: \begin{env}...\end{env}
    fn parse_latex_environment(&self, start_pos: usize) -> Option<(String, usize)> {
        if start_pos + 7 >= self.chars.len() {
            return None;
        }

        if self.chars[start_pos..start_pos + 7] != ['\\', 'b', 'e', 'g', 'i', 'n', '{'] {
            return None;
        }

        let (env_name, pos_after_name) = self.extract_environment_name(start_pos + 6)?;

        if !LATEX_ENVIRONMENTS.contains(&env_name.as_str()) {
            return None;
        }

        let end_pattern = format!("\\end{{{}}}", env_name);
        let end_chars: Vec<char> = end_pattern.chars().collect();

        let mut i = pos_after_name;
        let mut depth = 1;

        while i < self.chars.len() {
            if i + 7 + env_name.len() < self.chars.len() {
                let potential_begin = format!("\\begin{{{}}}", env_name);
                let begin_chars: Vec<char> = potential_begin.chars().collect();
                if i + begin_chars.len() <= self.chars.len()
                    && self.chars[i..i + begin_chars.len()] == begin_chars[..]
                {
                    depth += 1;
                    i += begin_chars.len();
                    continue;
                }
            }

            if i + end_chars.len() <= self.chars.len()
                && self.chars[i..i + end_chars.len()] == end_chars[..]
            {
                depth -= 1;
                if depth == 0 {
                    let content = self.slice(start_pos, i + end_chars.len()).to_string();
                    return Some((content, i + end_chars.len()));
                }
                i += end_chars.len();
                continue;
            }

            i += 1;
        }

        None
    }

    /// Run the full parse and return segments
    fn parse(self) -> Vec<MathSegment> {
        let mut segments = Vec::new();
        let mut i = 0;
        let mut current_text = String::new();

        while i < self.chars.len() {
            // Check for LaTeX environments: \begin{...}
            if i + 7 < self.chars.len()
                && self.chars[i] == '\\'
                && self.chars[i + 1..i + 6] == ['b', 'e', 'g', 'i', 'n']
                && self.chars[i + 6] == '{'
                && let Some((env_content, new_pos)) = self.parse_latex_environment(i)
            {
                if !current_text.is_empty() {
                    segments.push(MathSegment::Text(current_text.clone()));
                    current_text.clear();
                }

                segments.push(MathSegment::BlockMath(env_content));
                i = new_pos;
                continue;
            }

            // Check for LaTeX display math: \[
            if i + 1 < self.chars.len() && self.chars[i] == '\\' && self.chars[i + 1] == '[' {
                if !current_text.is_empty() {
                    segments.push(MathSegment::Text(current_text.clone()));
                    current_text.clear();
                }

                i += 2;
                let math_start = i;
                let mut found_close = false;

                while i + 1 < self.chars.len() {
                    if self.chars[i] == '\\' && self.chars[i + 1] == ']' {
                        let math_content = self.slice(math_start, i);
                        found_close = true;
                        i += 2;

                        if !math_content.trim().is_empty() {
                            segments.push(MathSegment::BlockMath(math_content.trim().to_string()));
                        }
                        break;
                    }
                    i += 1;
                }

                if !found_close {
                    current_text.push_str("\\[");
                    current_text.push_str(self.slice(math_start, i));
                }

                continue;
            }

            // Check for LaTeX inline math: \(
            if i + 1 < self.chars.len() && self.chars[i] == '\\' && self.chars[i + 1] == '(' {
                i += 2;
                let math_start = i;
                let mut found_close = false;

                while i + 1 < self.chars.len() {
                    if self.chars[i] == '\\' && self.chars[i + 1] == ')' {
                        let math_content = self.slice(math_start, i);
                        found_close = true;
                        i += 2;

                        if !math_content.trim().is_empty() {
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
                    current_text.push_str("\\(");
                    current_text.push_str(self.slice(math_start, i));
                }

                continue;
            }

            // Check for block math: ```math or ```latex
            if i + 7 < self.chars.len()
                && self.chars[i] == '`'
                && self.chars[i + 1] == '`'
                && self.chars[i + 2] == '`'
            {
                if let Some((math_content, new_pos)) = self.parse_math_code_block(i, "math") {
                    if !current_text.is_empty() {
                        segments.push(MathSegment::Text(current_text.clone()));
                        current_text.clear();
                    }

                    segments.push(MathSegment::BlockMath(math_content));
                    i = new_pos;
                    continue;
                }

                if let Some((math_content, new_pos)) = self.parse_math_code_block(i, "latex") {
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
            if i + 1 < self.chars.len()
                && self.chars[i] == '$'
                && self.chars[i + 1] == '$'
                && (i == 0 || self.chars[i - 1] != '\\')
            {
                let is_block_start = self.is_block_math_delimiter(i);

                let mut j = i + 2;
                let mut found_close = false;
                let mut has_newlines = false;

                while j + 1 < self.chars.len() {
                    if self.chars[j] == '\n' || self.chars[j] == '\r' {
                        has_newlines = true;
                    }

                    if self.chars[j] == '$' && self.chars[j + 1] == '$' {
                        if j > 0 && self.chars[j - 1] == '\\' {
                            j += 2;
                            continue;
                        }

                        let math_content = self.slice(i + 2, j);
                        found_close = true;

                        let mut is_block_end = false;
                        let mut end_pos = j + 2;
                        while end_pos < self.chars.len() {
                            if self.chars[end_pos] == '\n' || self.chars[end_pos] == '\r' {
                                is_block_end = true;
                                break;
                            }
                            if !self.chars[end_pos].is_whitespace() {
                                break;
                            }
                            end_pos += 1;
                        }
                        if end_pos >= self.chars.len() {
                            is_block_end = true;
                        }

                        if !math_content.trim().is_empty() {
                            if !current_text.is_empty() {
                                segments.push(MathSegment::Text(current_text.clone()));
                                current_text.clear();
                            }

                            if (is_block_start && is_block_end) || has_newlines {
                                segments
                                    .push(MathSegment::BlockMath(math_content.trim().to_string()));
                            } else {
                                segments
                                    .push(MathSegment::InlineMath(math_content.trim().to_string()));
                            }
                        }

                        i = j + 2;
                        break;
                    }
                    j += 1;
                }

                if !found_close {
                    current_text.push('$');
                    i += 1;
                    continue;
                }

                continue;
            }

            // Check for single $ (inline math)
            if self.chars[i] == '$' && (i == 0 || self.chars[i - 1] != '\\') {
                let mut j = i + 1;
                let mut found_close = false;

                while j < self.chars.len() {
                    if self.chars[j] == '$' {
                        if j > 0 && self.chars[j - 1] == '\\' {
                            j += 1;
                            continue;
                        }

                        found_close = true;
                        let math_content = self.slice(i + 1, j);

                        if !math_content.trim().is_empty() {
                            if !current_text.is_empty() {
                                segments.push(MathSegment::Text(current_text.clone()));
                                current_text.clear();
                            }

                            segments.push(MathSegment::InlineMath(math_content.trim().to_string()));
                        }

                        i = j + 1;
                        break;
                    }
                    j += 1;
                }

                if !found_close {
                    current_text.push(self.chars[i]);
                    i += 1;
                }

                continue;
            }

            // Regular character
            current_text.push(self.chars[i]);
            i += 1;
        }

        if !current_text.is_empty() {
            segments.push(MathSegment::Text(current_text));
        }

        segments
    }
}

/// Parse content into segments of text and math expressions
pub fn parse_math_segments(content: &str) -> Vec<MathSegment> {
    MathParser::new(content).parse()
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
