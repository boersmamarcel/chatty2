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
