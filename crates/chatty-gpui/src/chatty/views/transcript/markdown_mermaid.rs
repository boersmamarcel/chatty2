//! Splitting a markdown document around its mermaid diagrams, so the
//! artifact panel can draw each diagram and hand the rest to `TextView`
//! (AGE-566). Pure: no gpui, no renderer.

/// One run of a markdown document: prose (including every non-mermaid code
/// block, verbatim) or the source of one ```` ```mermaid ```` diagram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DocPart {
    Markdown(String),
    Mermaid(String),
}

/// An open fence: its character, run length, and whether it is mermaid.
struct Fence {
    ch: char,
    len: usize,
    mermaid: bool,
}

/// A fence line's (char, run length, info string), per CommonMark: up to three
/// spaces of indent, three or more backticks or tildes. A backtick fence's
/// info string may not contain a backtick.
fn fence_marker(line: &str) -> Option<(char, usize, &str)> {
    let trimmed = line.trim_end_matches(['\n', '\r']);
    let indent = trimmed.len() - trimmed.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &trimmed[indent..];
    let ch = rest.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = rest.chars().take_while(|c| *c == ch).count();
    if len < 3 {
        return None;
    }
    let info = rest[len..].trim();
    if ch == '`' && info.contains('`') {
        return None;
    }
    Some((ch, len, info))
}

/// Split `text` into prose and mermaid diagrams, in document order.
///
/// Fence-aware: a mermaid fence quoted inside another code block stays part
/// of that block, and an unclosed mermaid fence stays markdown rather than
/// swallowing the rest of the document. Only top-level fences count; one in a
/// list item or block quote is left to `TextView`.
pub(crate) fn split_mermaid(text: &str) -> Vec<DocPart> {
    let mut parts = Vec::new();
    let mut prose = String::new();
    let mut diagram = String::new();
    // The fence line that opened `diagram`, restored if it never closes.
    let mut opener = String::new();
    let mut open: Option<Fence> = None;

    for line in text.split_inclusive('\n') {
        match &open {
            None => {
                if let Some((ch, len, info)) = fence_marker(line) {
                    let mermaid = info.split_whitespace().next() == Some("mermaid");
                    open = Some(Fence { ch, len, mermaid });
                    if mermaid {
                        opener = line.to_string();
                        continue;
                    }
                }
                prose.push_str(line);
            }
            Some(fence) => {
                let closes = matches!(
                    fence_marker(line),
                    Some((ch, len, info)) if ch == fence.ch && len >= fence.len && info.is_empty()
                );
                if fence.mermaid {
                    if closes {
                        if !prose.is_empty() {
                            parts.push(DocPart::Markdown(std::mem::take(&mut prose)));
                        }
                        parts.push(DocPart::Mermaid(std::mem::take(&mut diagram)));
                        opener.clear();
                    } else {
                        diagram.push_str(line);
                    }
                } else {
                    prose.push_str(line);
                }
                if closes {
                    open = None;
                }
            }
        }
    }

    if matches!(open, Some(Fence { mermaid: true, .. })) {
        prose.push_str(&opener);
        prose.push_str(&diagram);
    }
    if !prose.is_empty() {
        parts.push(DocPart::Markdown(prose));
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(s: &str) -> DocPart {
        DocPart::Markdown(s.to_string())
    }
    fn mm(s: &str) -> DocPart {
        DocPart::Mermaid(s.to_string())
    }

    #[test]
    fn plain_markdown_is_one_part() {
        let text = "# Title\n\nSome *text*.\n";
        assert_eq!(split_mermaid(text), vec![md(text)]);
    }

    #[test]
    fn diagrams_are_split_out_in_order() {
        let text = "# A\n\n```mermaid\nflowchart LR\n  a --> b\n```\n\nMid\n\n~~~mermaid\nsequenceDiagram\n~~~\nEnd\n";
        assert_eq!(
            split_mermaid(text),
            vec![
                md("# A\n\n"),
                mm("flowchart LR\n  a --> b\n"),
                md("\nMid\n\n"),
                mm("sequenceDiagram\n"),
                md("End\n"),
            ]
        );
    }

    #[test]
    fn other_code_blocks_stay_markdown_verbatim() {
        let text = "```rust\nfn main() {}\n```\n";
        assert_eq!(split_mermaid(text), vec![md(text)]);
    }

    #[test]
    fn mermaid_fence_quoted_inside_another_block_is_not_a_diagram() {
        let text = "````markdown\n```mermaid\nflowchart LR\n```\n````\n";
        assert_eq!(split_mermaid(text), vec![md(text)]);
    }

    #[test]
    fn unclosed_mermaid_fence_stays_markdown() {
        let text = "Intro\n```mermaid\nflowchart LR\n  a --> b\n";
        assert_eq!(split_mermaid(text), vec![md(text)]);
    }

    #[test]
    fn closing_fence_must_match_and_be_long_enough() {
        // A tilde line and a shorter run don't close a four-backtick fence.
        let text = "````mermaid\nflowchart LR\n~~~\n```\n````\n";
        assert_eq!(split_mermaid(text), vec![mm("flowchart LR\n~~~\n```\n")]);
    }

    #[test]
    fn info_string_after_mermaid_is_allowed_and_crlf_is_handled() {
        let text = "```mermaid title\r\ngraph TD\r\n```\r\n";
        assert_eq!(split_mermaid(text), vec![mm("graph TD\r\n")]);
    }

    #[test]
    fn indented_code_is_not_a_fence() {
        let text = "    ```mermaid\n    graph TD\n    ```\n";
        assert_eq!(split_mermaid(text), vec![md(text)]);
    }
}
