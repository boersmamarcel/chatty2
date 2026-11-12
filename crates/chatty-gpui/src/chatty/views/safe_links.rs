//! Links in model and tool output that a click may open (AGE-1262).
//!
//! gpui-component's `TextView` hands a clicked link's URL straight to the
//! operating system (`cx.open_url`), with no scheme check. Model output is
//! steerable by anything the agent reads, so a link such as
//! `[Open the report](file:///…/report.command)` or a custom scheme
//! (`ms-msdt:`, `vscode://`) would run code from one click.
//!
//! [`neuter_unsafe_links`] rewrites the markdown before it reaches
//! `TextView`: a link, image or link definition whose target is not
//! `http`, `https` or `mailto` becomes plain text that shows the target in
//! a code span, so the user still sees it but a click opens nothing.
//! Relative targets count as unsafe too: the OS opener would resolve them
//! against the app's working directory.

use markdown::ParseOptions;
use markdown::mdast::Node;
use std::borrow::Cow;

/// Schemes a click may hand to the OS opener.
const SAFE_SCHEMES: &[&str] = &["http", "https", "mailto"];

/// Whether `url` may be opened by a click.
pub(crate) fn is_safe_link(url: &str) -> bool {
    let url = url.trim();
    let Some((scheme, _)) = url.split_once(':') else {
        return false;
    };
    // A scheme is ASCII letters, digits, `+`, `-`, `.`; anything else before
    // the first `:` means there is no scheme (e.g. `./a:b`).
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return false;
    }
    SAFE_SCHEMES
        .iter()
        .any(|safe| scheme.eq_ignore_ascii_case(safe))
}

/// `text` with every unsafe link rewritten to plain text (see the module
/// docs). Borrowed when nothing needed to change.
pub(crate) fn neuter_unsafe_links(text: &str) -> Cow<'_, str> {
    // Cheap exit: no inline link, definition or autolink syntax at all.
    if !(text.contains("](") || text.contains("]:") || text.contains('<')) {
        return Cow::Borrowed(text);
    }
    // Same options gpui-component parses with, so the spans line up.
    let Ok(root) = markdown::to_mdast(text, &ParseOptions::gfm()) else {
        return Cow::Borrowed(text);
    };
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    collect(&root, &mut edits);
    if edits.is_empty() {
        return Cow::Borrowed(text);
    }
    edits.sort_by_key(|(start, _, _)| *start);
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (start, end, replacement) in edits {
        // Nested edits cannot happen (an unsafe node's children are not
        // visited), but stay safe against overlapping spans.
        if start < at || end > text.len() {
            continue;
        }
        out.push_str(&text[at..start]);
        out.push_str(&replacement);
        at = end;
    }
    out.push_str(&text[at..]);
    Cow::Owned(out)
}

fn collect(node: &Node, edits: &mut Vec<(usize, usize, String)>) {
    let unsafe_target = match node {
        Node::Link(link) if !is_safe_link(&link.url) => Some((node.to_string(), &link.url)),
        Node::Image(image) if !is_safe_link(&image.url) => Some((image.alt.clone(), &image.url)),
        Node::Definition(def) if !is_safe_link(&def.url) => Some((String::new(), &def.url)),
        _ => None,
    };
    if let Some((label, url)) = unsafe_target {
        if let Some(pos) = node.position() {
            edits.push((pos.start.offset, pos.end.offset, plain(&label, url)));
        }
        return;
    }
    if let Some(children) = node.children() {
        for child in children {
            collect(child, edits);
        }
    }
}

/// `label (`url`)` with the label's markdown escaped and the URL in a code
/// span, so neither can form a new link.
fn plain(label: &str, url: &str) -> String {
    let mut out = String::new();
    for c in label.chars() {
        if c.is_ascii_punctuation() {
            out.push('\\');
        }
        out.push(c);
    }
    let url: String = url.chars().filter(|c| *c != '`' && *c != '\n').collect();
    if !out.is_empty() {
        out.push(' ');
    }
    out.push_str("(`");
    out.push_str(&url);
    out.push_str("`)");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links_in(text: &str) -> Vec<String> {
        fn walk(node: &Node, out: &mut Vec<String>) {
            match node {
                Node::Link(l) => out.push(l.url.clone()),
                Node::Image(i) => out.push(i.url.clone()),
                Node::Definition(d) => out.push(d.url.clone()),
                _ => {}
            }
            if let Some(children) = node.children() {
                for c in children {
                    walk(c, out);
                }
            }
        }
        let root = markdown::to_mdast(text, &ParseOptions::gfm()).unwrap();
        let mut out = Vec::new();
        walk(&root, &mut out);
        out
    }

    #[test]
    fn safe_schemes_are_kept() {
        let text = "See [docs](https://example.com/a) or [mail](mailto:a@b.c) or <http://x.y>.";
        assert_eq!(neuter_unsafe_links(text), text);
    }

    #[test]
    fn unsafe_schemes_become_plain_text() {
        for url in [
            "file:///tmp/report.command",
            "FILE:///tmp/x",
            "javascript:alert(1)",
            "ms-msdt:/id PCWDiagnostic",
            "vscode://file/x",
            "report.command",
            "./a:b",
        ] {
            let text = format!("Click [Open the report]({url}) now.");
            let out = neuter_unsafe_links(&text);
            assert!(links_in(&out).is_empty(), "{url}: still a link in {out:?}");
            assert!(out.contains("Open the report"), "{out:?}");
        }
    }

    #[test]
    fn autolinks_images_and_definitions_are_neutered() {
        let text =
            "<file:///etc/passwd>\n\n![x](file:///a.png)\n\n[r][d]\n\n[d]: file:///x.command\n";
        let out = neuter_unsafe_links(text);
        assert!(links_in(&out).is_empty(), "{out:?}");
    }

    #[test]
    fn mixed_links_keep_only_the_safe_one() {
        let text = "[a](https://ok.example) and [b](file:///bad)";
        let out = neuter_unsafe_links(text);
        assert_eq!(links_in(&out), vec!["https://ok.example".to_string()]);
    }
}
