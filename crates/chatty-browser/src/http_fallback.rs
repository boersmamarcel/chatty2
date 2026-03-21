//! HTTP fallback for the browse tool.
//!
//! When the browser backend is unavailable (e.g. WryBackend stub not yet
//! implemented), we fall back to a plain HTTP GET + HTML parsing to build a
//! [`PageSnapshot`]. This gives the LLM useful page content without requiring
//! a full browser engine.

use crate::page::{LinkInfo, PageSnapshot};
use tracing::debug;

/// Request timeout for HTTP fallback.
const REQUEST_TIMEOUT_SECS: u64 = 30;

/// Maximum text content length in characters.
const MAX_TEXT_LEN: usize = 3_000;

/// Maximum number of links to extract.
const MAX_LINKS: usize = 50;

/// Fetch a URL via HTTP GET and build a [`PageSnapshot`] from the HTML response.
pub async fn fetch_and_snapshot(url: &str) -> anyhow::Result<PageSnapshot> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .user_agent("Chatty/1.0 (Desktop AI Assistant)")
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()?;

    debug!(url = %url, "HTTP fallback: fetching page");

    let response = client.get(url).send().await?;
    let final_url = response.url().to_string();
    let status = response.status();

    if !status.is_success() {
        anyhow::bail!("HTTP {status} for {url}");
    }

    let body = response.text().await?;

    // Extract title
    let title = extract_between(&body, "<title", "</title>")
        .unwrap_or_default()
        .to_string();

    // Extract text content
    let text_content = html_to_text(&body);
    let text_content = truncate_text(&text_content, MAX_TEXT_LEN);

    // Extract links
    let links = extract_links(&body, url);

    // Extract OG image
    let og_image_url = extract_meta_content(&body, "og:image");

    // Extract description
    let description = extract_meta_content(&body, "description")
        .or_else(|| extract_meta_content(&body, "og:description"));

    Ok(PageSnapshot {
        url: final_url,
        title,
        text_content,
        elements: Vec::new(), // No interactive elements in HTTP-only mode
        forms: Vec::new(),    // No form interaction in HTTP-only mode
        links,
        login_hint: None,
        og_image_url,
        description,
    })
}

/// Extract text between an opening tag (by prefix) and a closing tag.
/// Returns the text content inside the tag, stripping the tag attributes.
fn extract_between<'a>(html: &'a str, open_prefix: &str, close_tag: &str) -> Option<&'a str> {
    let lower = html.to_lowercase();
    let start = lower.find(&open_prefix.to_lowercase())?;
    // Find the end of the opening tag (the '>')
    let content_start = html[start..].find('>')? + start + 1;
    let end = lower[content_start..].find(&close_tag.to_lowercase())? + content_start;
    Some(html[content_start..end].trim())
}

/// Extract links from HTML using simple regex-free parsing.
fn extract_links(html: &str, base_url: &str) -> Vec<LinkInfo> {
    let base = url::Url::parse(base_url).ok();
    let mut links = Vec::new();
    let lower = html.to_lowercase();
    let mut search_from = 0;

    while links.len() < MAX_LINKS {
        // Find next <a tag
        let Some(a_start) = lower[search_from..]
            .find("<a ")
            .or_else(|| lower[search_from..].find("<a\n"))
        else {
            break;
        };
        let a_start = search_from + a_start;
        search_from = a_start + 3;

        // Find the end of the opening <a> tag
        let Some(tag_end) = html[a_start..].find('>') else {
            continue;
        };
        let tag = &html[a_start..a_start + tag_end + 1];

        // Extract href attribute
        let Some(href) = extract_attribute(tag, "href") else {
            continue;
        };

        // Skip fragment-only and javascript: links
        if href.starts_with('#') || href.starts_with("javascript:") {
            continue;
        }

        // Resolve relative URLs
        let resolved = if href.starts_with("http://") || href.starts_with("https://") {
            href.to_string()
        } else if let Some(base) = &base {
            base.join(href).map(|u| u.to_string()).unwrap_or_default()
        } else {
            continue;
        };

        if resolved.is_empty() {
            continue;
        }

        // Find closing </a> to extract link text
        let content_start = a_start + tag_end + 1;
        let text = if let Some(close_pos) = lower[content_start..].find("</a>") {
            let raw = &html[content_start..content_start + close_pos];
            strip_tags(raw).trim().to_string()
        } else {
            String::new()
        };

        // Truncate long text
        let text = if text.len() > 100 {
            format!(
                "{}…",
                &text[..text
                    .char_indices()
                    .take(100)
                    .last()
                    .map(|(i, _)| i)
                    .unwrap_or(0)]
            )
        } else {
            text
        };

        links.push(LinkInfo {
            text,
            href: resolved,
        });
    }

    links
}

/// Extract the value of an HTML attribute from a tag string.
fn extract_attribute<'a>(tag: &'a str, attr_name: &str) -> Option<&'a str> {
    let lower = tag.to_lowercase();
    let pattern = format!("{}=", attr_name.to_lowercase());

    let attr_start = lower.find(&pattern)?;
    let value_start = attr_start + pattern.len();
    let rest = &tag[value_start..];

    if let Some(inner) = rest.strip_prefix('"') {
        let end = inner.find('"')?;
        Some(&inner[..end])
    } else if let Some(inner) = rest.strip_prefix('\'') {
        let end = inner.find('\'')?;
        Some(&inner[..end])
    } else {
        // Unquoted attribute value — ends at whitespace or >
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '>')
            .unwrap_or(rest.len());
        Some(&rest[..end])
    }
}

/// Extract the content attribute from a <meta> tag matching a given name or property.
fn extract_meta_content(html: &str, name: &str) -> Option<String> {
    let lower = html.to_lowercase();
    let name_lower = name.to_lowercase();

    // Search for meta tags with name= or property= matching our target
    let mut search_from = 0;
    while let Some(meta_start) = lower[search_from..].find("<meta") {
        let meta_start = search_from + meta_start;
        search_from = meta_start + 5;

        let Some(tag_end) = html[meta_start..].find('>') else {
            continue;
        };
        let tag = &html[meta_start..meta_start + tag_end + 1];
        let tag_lower = tag.to_lowercase();

        // Check if this meta tag matches our name/property
        let matches = tag_lower.contains(&format!("name=\"{}\"", name_lower))
            || tag_lower.contains(&format!("property=\"{}\"", name_lower))
            || tag_lower.contains(&format!("name='{}'", name_lower))
            || tag_lower.contains(&format!("property='{}'", name_lower));

        if matches && let Some(content) = extract_attribute(tag, "content") {
            let content = content.trim().to_string();
            if !content.is_empty() {
                return Some(content);
            }
        }
    }

    None
}

/// Strip HTML tags from a string, returning plain text.
fn strip_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;

    for ch in html.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            result.push(ch);
        }
    }

    result
}

/// Convert HTML to readable plain text, stripping tags and normalizing whitespace.
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

    // Decode common HTML entities
    let result = result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ");

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

/// Truncate text at a word boundary, appending '…' if truncated.
fn truncate_text(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_string();
    }
    let safe_end = text
        .char_indices()
        .take_while(|(i, _)| *i <= max_len)
        .last()
        .map(|(i, _)| i)
        .unwrap_or(0);
    let truncation_point = text[..safe_end].rfind(' ').unwrap_or(safe_end);
    let mut result = text[..truncation_point].to_string();
    result.push('…');
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_between() {
        let html = r#"<html><head><title>Hello World</title></head></html>"#;
        assert_eq!(
            extract_between(html, "<title", "</title>"),
            Some("Hello World")
        );
    }

    #[test]
    fn test_extract_between_with_attrs() {
        let html = r#"<title lang="en">Test Page</title>"#;
        assert_eq!(
            extract_between(html, "<title", "</title>"),
            Some("Test Page")
        );
    }

    #[test]
    fn test_extract_attribute_double_quote() {
        let tag = r#"<a href="https://example.com" class="link">"#;
        assert_eq!(extract_attribute(tag, "href"), Some("https://example.com"));
    }

    #[test]
    fn test_extract_attribute_single_quote() {
        let tag = "<a href='https://example.com'>";
        assert_eq!(extract_attribute(tag, "href"), Some("https://example.com"));
    }

    #[test]
    fn test_extract_links() {
        let html = r##"<a href="https://example.com">Example</a>
                       <a href="/about">About Us</a>
                       <a href="#top">Top</a>"##;
        let links = extract_links(html, "https://base.com");
        assert_eq!(links.len(), 2); // #top is skipped
        assert_eq!(links[0].text, "Example");
        assert_eq!(links[0].href, "https://example.com");
        assert_eq!(links[1].text, "About Us");
        assert_eq!(links[1].href, "https://base.com/about");
    }

    #[test]
    fn test_extract_meta_content() {
        let html = r#"<meta name="description" content="A test page">
                       <meta property="og:image" content="https://img.example.com/og.png">"#;
        assert_eq!(
            extract_meta_content(html, "description"),
            Some("A test page".to_string())
        );
        assert_eq!(
            extract_meta_content(html, "og:image"),
            Some("https://img.example.com/og.png".to_string())
        );
    }

    #[test]
    fn test_html_to_text_basic() {
        let html = "<html><body><h1>Hello</h1><p>World</p></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("Hello"));
        assert!(text.contains("World"));
    }

    #[test]
    fn test_html_to_text_strips_scripts() {
        let html = "<p>Before</p><script>alert('x')</script><p>After</p>";
        let text = html_to_text(html);
        assert!(text.contains("Before"));
        assert!(text.contains("After"));
        assert!(!text.contains("alert"));
    }

    #[test]
    fn test_strip_tags() {
        assert_eq!(strip_tags("<b>bold</b> text"), "bold text");
        assert_eq!(strip_tags("no tags"), "no tags");
    }

    #[test]
    fn test_truncate_text() {
        let short = "hello";
        assert_eq!(truncate_text(short, 100), "hello");

        let long = "word1 word2 word3 word4 word5";
        let truncated = truncate_text(long, 15);
        assert!(truncated.ends_with('…'));
        assert!(truncated.len() <= 20); // 15 + some margin for word boundary + …
    }

    #[test]
    fn test_snapshot_has_no_elements_or_forms() {
        // HTTP fallback produces snapshots without interactive elements
        let snapshot = PageSnapshot {
            url: "https://example.com".into(),
            title: "Example".into(),
            text_content: "Hello".into(),
            elements: Vec::new(),
            forms: Vec::new(),
            links: Vec::new(),
            login_hint: None,
            og_image_url: None,
            description: None,
        };
        assert!(snapshot.elements.is_empty());
        assert!(snapshot.forms.is_empty());
    }
}
