//! HTTP-based fallback for when the Verso browser engine is unavailable.
//!
//! When `versoview` is not installed, the browse tool falls back to a plain
//! HTTP GET request, converts the HTML to text, and extracts links to build a
//! [`PageSnapshot`]. This gives the LLM useful page content without requiring
//! an external browser binary.

use crate::page_repr::{LinkInfo, PageSnapshot, PageState};

/// Maximum response body size for the HTTP fallback (5 MB).
const MAX_BODY_BYTES: usize = 5_000_000;

/// HTTP request timeout for the fallback path.
const HTTP_TIMEOUT_SECS: u64 = 30;

/// Maximum number of HTTP redirects to follow.
const MAX_REDIRECTS: usize = 10;

/// Fetch a URL via plain HTTP and build a [`PageSnapshot`] from the response.
///
/// This is intentionally simple: no JavaScript execution, no interactive
/// element detection, no form parsing. It converts HTML to readable text and
/// extracts `<a>` links so the LLM can navigate further.
pub async fn fetch_and_snapshot(url: &str) -> Result<PageSnapshot, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
        .user_agent("Chatty/1.0 (Desktop AI Assistant)")
        .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {}", e))?;

    let final_url = response.url().to_string();
    let status = response.status();

    if !status.is_success() {
        return Err(format!("HTTP {} for {}", status, url));
    }

    let body = response
        .text()
        .await
        .map_err(|e| format!("Failed to read response body: {}", e))?;

    // Truncate very large pages
    let body = if body.len() > MAX_BODY_BYTES {
        body[..MAX_BODY_BYTES].to_string()
    } else {
        body
    };

    let title = extract_title(&body).unwrap_or_else(|| url.to_string());
    let links = extract_links(&body, url);
    let text_content = html_to_text(&body);

    Ok(PageSnapshot {
        url: final_url,
        title,
        text_content,
        elements: vec![], // No interactive element detection without JS
        forms: vec![],    // No form detection without DOM access
        links,
        state: PageState::Complete,
    })
}

/// Extract the `<title>` content from HTML.
fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let tag = "<title";
    let start = lower.find(tag)?.checked_add(tag.len())?;
    // Skip past '>' to reach title content
    let after_open = lower[start..]
        .find('>')?
        .checked_add(start)?
        .checked_add(1)?;
    let end_offset = lower[after_open..].find("</title")?;
    let end = after_open.checked_add(end_offset)?;
    let raw = &html[after_open..end];
    let text = raw.trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(decode_entities(&text))
    }
}

/// Extract `<a href="...">text</a>` links from HTML.
fn extract_links(html: &str, base_url: &str) -> Vec<LinkInfo> {
    let mut links = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut search_from = 0;

    while let Some(a_pos) = lower[search_from..].find("<a ") {
        let abs_pos = search_from + a_pos;
        let tag_end = match lower[abs_pos..].find('>') {
            Some(p) => abs_pos + p,
            None => break,
        };

        // Extract href from the tag
        let tag_content = &html[abs_pos..tag_end + 1];
        let href = extract_attr(tag_content, "href");

        // Extract link text (content between > and </a>)
        let text_start = tag_end + 1;
        let close_offset = match lower[text_start..].find("</a") {
            Some(p) => p,
            None => {
                search_from = tag_end + 1;
                continue;
            }
        };
        let text_end = text_start + close_offset;
        let link_text = html_to_text(&html[text_start..text_end]);

        if let Some(href) = href {
            let href = decode_entities(&href);
            // Skip fragment-only and javascript: links
            if !href.is_empty()
                && !href.starts_with('#')
                && !href.starts_with("javascript:")
                && !href.starts_with("mailto:")
            {
                let resolved = resolve_url(&href, base_url);
                if !link_text.trim().is_empty() {
                    links.push(LinkInfo {
                        text: link_text.trim().to_string(),
                        href: resolved,
                    });
                }
            }
        }

        search_from = text_end + "</a>".len(); // skip past closing tag
    }

    links
}

/// Extract an attribute value from an HTML tag string.
fn extract_attr(tag: &str, attr_name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let needle = format!("{}=", attr_name);
    let pos = lower.find(&needle)?;
    let after_eq = pos + needle.len();
    let rest = &tag[after_eq..];

    if rest.starts_with('"') {
        let end = rest[1..].find('"')?;
        Some(rest[1..1 + end].to_string())
    } else if rest.starts_with('\'') {
        let end = rest[1..].find('\'')?;
        Some(rest[1..1 + end].to_string())
    } else {
        // Unquoted: take until whitespace or >
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '>')
            .unwrap_or(rest.len());
        Some(rest[..end].to_string())
    }
}

/// Resolve a potentially relative URL against a base URL.
fn resolve_url(href: &str, base: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    if href.starts_with("//") {
        let scheme = if base.starts_with("https") {
            "https:"
        } else {
            "http:"
        };
        return format!("{}{}", scheme, href);
    }

    // Extract base components
    let base_no_fragment = base.split('#').next().unwrap_or(base);
    if href.starts_with('/') {
        // Absolute path — attach to origin
        if let Some(origin_end) = base_no_fragment
            .find("://")
            .and_then(|s| base_no_fragment[s + 3..].find('/').map(|p| s + 3 + p))
        {
            return format!("{}{}", &base_no_fragment[..origin_end], href);
        }
        return format!("{}{}", base_no_fragment.trim_end_matches('/'), href);
    }

    // Relative path — resolve against base directory
    let base_dir = if let Some(last_slash) = base_no_fragment.rfind('/') {
        &base_no_fragment[..last_slash + 1]
    } else {
        base_no_fragment
    };
    format!("{}{}", base_dir, href)
}

/// Convert HTML to readable plain text by stripping tags, script/style blocks,
/// normalizing whitespace, and decoding HTML entities.
fn html_to_text(html: &str) -> String {
    let mut result = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut last_was_whitespace = false;
    let mut tag_name = String::new();
    let mut collecting_tag_name = false;

    for ch in html.chars() {
        if ch == '<' {
            in_tag = true;
            collecting_tag_name = true;
            tag_name.clear();
            continue;
        }

        if in_tag {
            if collecting_tag_name {
                if ch.is_alphanumeric() || ch == '/' {
                    tag_name.push(ch.to_ascii_lowercase());
                } else {
                    collecting_tag_name = false;
                }
            }
            if ch == '>' {
                in_tag = false;
                collecting_tag_name = false;

                if tag_name == "script" {
                    in_script = true;
                } else if tag_name == "/script" {
                    in_script = false;
                } else if tag_name == "style" {
                    in_style = true;
                } else if tag_name == "/style" {
                    in_style = false;
                }

                let is_block = matches!(
                    tag_name.trim_start_matches('/'),
                    "p" | "div"
                        | "br"
                        | "br/"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "li"
                        | "tr"
                        | "blockquote"
                        | "pre"
                        | "hr"
                        | "header"
                        | "footer"
                        | "section"
                        | "article"
                        | "nav"
                        | "main"
                );
                if is_block && !result.ends_with('\n') {
                    result.push('\n');
                    last_was_whitespace = true;
                }
            }
            continue;
        }

        if in_script || in_style {
            continue;
        }

        if ch.is_whitespace() {
            if !last_was_whitespace {
                result.push(' ');
                last_was_whitespace = true;
            }
        } else {
            result.push(ch);
            last_was_whitespace = false;
        }
    }

    let result = decode_entities(&result);

    // Clean up excessive newlines
    let mut cleaned = String::with_capacity(result.len());
    let mut consecutive_newlines = 0;
    for ch in result.chars() {
        if ch == '\n' {
            consecutive_newlines += 1;
            if consecutive_newlines <= 2 {
                cleaned.push(ch);
            }
        } else {
            consecutive_newlines = 0;
            cleaned.push(ch);
        }
    }

    cleaned.trim().to_string()
}

/// Decode common HTML entities.
fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_html_to_text_basic() {
        let html = "<html><body><h1>Hello</h1><p>World</p></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("Hello"));
        assert!(text.contains("World"));
    }

    #[test]
    fn test_html_to_text_strips_script() {
        let html = "<p>Before</p><script>var x = 1;</script><p>After</p>";
        let text = html_to_text(html);
        assert!(text.contains("Before"));
        assert!(text.contains("After"));
        assert!(!text.contains("var x"));
    }

    #[test]
    fn test_html_to_text_strips_style() {
        let html = "<p>Visible</p><style>.hidden { display: none; }</style><p>Also visible</p>";
        let text = html_to_text(html);
        assert!(text.contains("Visible"));
        assert!(text.contains("Also visible"));
        assert!(!text.contains("display"));
    }

    #[test]
    fn test_html_to_text_decodes_entities() {
        let html = "<p>A &amp; B &lt; C &gt; D</p>";
        let text = html_to_text(html);
        assert!(text.contains("A & B < C > D"));
    }

    #[test]
    fn test_extract_title() {
        assert_eq!(
            extract_title("<html><head><title>My Page</title></head></html>"),
            Some("My Page".to_string())
        );
        assert_eq!(extract_title("<html><body>No title</body></html>"), None);
    }

    #[test]
    fn test_extract_title_with_entities() {
        assert_eq!(
            extract_title("<title>A &amp; B</title>"),
            Some("A & B".to_string())
        );
    }

    #[test]
    fn test_extract_links() {
        let html = r#"<a href="https://example.com">Example</a><a href="/about">About</a>"#;
        let links = extract_links(html, "https://test.com/page");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].text, "Example");
        assert_eq!(links[0].href, "https://example.com");
        assert_eq!(links[1].text, "About");
        assert_eq!(links[1].href, "https://test.com/about");
    }

    #[test]
    fn test_extract_links_skips_javascript() {
        let html = r#"<a href="javascript:void(0)">Click</a><a href="https://real.com">Real</a>"#;
        let links = extract_links(html, "https://test.com");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].text, "Real");
    }

    #[test]
    fn test_extract_links_skips_fragments() {
        let html = r##"<a href="#top">Top</a><a href="https://real.com">Real</a>"##;
        let links = extract_links(html, "https://test.com");
        assert_eq!(links.len(), 1);
    }

    #[test]
    fn test_resolve_url_absolute() {
        assert_eq!(
            resolve_url("https://other.com/page", "https://base.com/"),
            "https://other.com/page"
        );
    }

    #[test]
    fn test_resolve_url_relative() {
        assert_eq!(
            resolve_url("page2.html", "https://base.com/dir/page1.html"),
            "https://base.com/dir/page2.html"
        );
    }

    #[test]
    fn test_resolve_url_absolute_path() {
        assert_eq!(
            resolve_url("/about", "https://base.com/dir/page"),
            "https://base.com/about"
        );
    }

    #[test]
    fn test_resolve_url_protocol_relative() {
        assert_eq!(
            resolve_url("//cdn.example.com/file.js", "https://base.com/"),
            "https://cdn.example.com/file.js"
        );
    }

    #[test]
    fn test_extract_attr() {
        assert_eq!(
            extract_attr(r#"<a href="https://example.com" class="link">"#, "href"),
            Some("https://example.com".to_string())
        );
        assert_eq!(
            extract_attr(r#"<a href='single.html'>"#, "href"),
            Some("single.html".to_string())
        );
    }
}
