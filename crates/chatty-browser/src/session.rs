//! Browser session — represents a single browsing context (tab).
//!
//! Each `BrowserSession` wraps a DevTools client and tracks page state.
//! Sessions are created via [`BrowserEngine::create_session`].

use crate::devtools::DevToolsClient;
use crate::error::BrowserError;
use crate::page_repr::{
    FormField, FormInfo, InteractiveElement, LinkInfo, PageSnapshot, PageState,
};
use std::sync::Arc;
use tracing::debug;

/// Maximum number of interactive elements extracted per page snapshot.
const MAX_INTERACTIVE_ELEMENTS: usize = 100;
/// Maximum number of forms extracted per page snapshot.
const MAX_FORMS: usize = 10;
/// Maximum number of links extracted per page snapshot.
const MAX_LINKS: usize = 50;

/// A single browsing session (one tab/context) connected to the Verso engine.
pub struct BrowserSession {
    /// Unique session identifier.
    id: String,
    /// DevTools client shared with the engine.
    devtools: Arc<DevToolsClient>,
    /// The DevTools actor ID for this tab (static default for Verso's DevTools protocol).
    actor_id: String,
    /// Current page URL.
    current_url: Option<String>,
    /// Page load timeout in milliseconds.
    page_load_timeout_ms: u64,
    /// Counter for generating stable element IDs within this session.
    next_element_id: u64,
}

impl BrowserSession {
    /// Create a new session.
    pub(crate) fn new(
        id: String,
        devtools: Arc<DevToolsClient>,
        page_load_timeout_ms: u64,
    ) -> Self {
        Self {
            id,
            devtools,
            actor_id: "tab1".to_string(), // Default actor; updated on first navigation
            current_url: None,
            page_load_timeout_ms,
            next_element_id: 1,
        }
    }

    /// Get the session ID.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Get the current page URL, if any.
    pub fn current_url(&self) -> Option<&str> {
        self.current_url.as_deref()
    }

    /// Navigate to a URL and return a snapshot of the loaded page.
    pub async fn navigate(&mut self, url: &str) -> Result<PageSnapshot, BrowserError> {
        debug!(url, session = %self.id, "Navigating");

        let response = self.devtools.navigate(url, &self.actor_id).await?;

        if let Some(err) = response.error {
            return Err(BrowserError::NavigationFailed(err));
        }

        self.current_url = Some(url.to_string());

        // Wait for the page to load, then build a snapshot
        self.wait_for_page_load().await?;
        self.build_page_snapshot().await
    }

    /// Evaluate JavaScript in the page context.
    pub async fn evaluate_js(&self, expression: &str) -> Result<String, BrowserError> {
        let result = self
            .devtools
            .evaluate_js(expression, &self.actor_id)
            .await?;

        if result.is_exception {
            return Err(BrowserError::JsEvalError(result.value));
        }

        Ok(result.value)
    }

    /// Build a structured snapshot of the current page by evaluating JavaScript.
    pub async fn build_page_snapshot(&mut self) -> Result<PageSnapshot, BrowserError> {
        // Reset element ID counter for each snapshot
        self.next_element_id = 1;

        // Extract page metadata
        let title = self
            .evaluate_js("document.title")
            .await
            .unwrap_or_else(|_| String::new());
        let url = self
            .evaluate_js("window.location.href")
            .await
            .unwrap_or_else(|_| {
                self.current_url
                    .clone()
                    .unwrap_or_else(|| "about:blank".to_string())
            });

        // Extract page text content (simplified)
        let text_content = self
            .evaluate_js(
                r#"(function() {
                    var body = document.body;
                    if (!body) return '';
                    // Remove script and style elements from the clone
                    var clone = body.cloneNode(true);
                    var scripts = clone.querySelectorAll('script, style, noscript');
                    for (var i = 0; i < scripts.length; i++) scripts[i].remove();
                    return clone.innerText || clone.textContent || '';
                })()"#,
            )
            .await
            .unwrap_or_default();

        // Extract interactive elements
        let elements_js = format!(
            r#"(function() {{
                    var els = document.querySelectorAll(
                        'a, button, input, select, textarea, [role="button"], [role="link"], [tabindex]'
                    );
                    var result = [];
                    for (var i = 0; i < els.length && i < {max_elements}; i++) {{
                        var el = els[i];
                        var rect = el.getBoundingClientRect();
                        result.push({{
                            tag: el.tagName.toLowerCase(),
                            role: el.getAttribute('role'),
                            text: (el.textContent || el.value || el.placeholder || el.getAttribute('aria-label') || '').trim().substring(0, 100),
                            type: el.type || null,
                            selector: el.id ? '#' + el.id : (el.name ? '[name="' + el.name + '"]' : el.tagName.toLowerCase()),
                            visible: rect.width > 0 && rect.height > 0,
                            enabled: !el.disabled
                        }});
                    }}
                    return JSON.stringify(result);
                }})()"#,
            max_elements = MAX_INTERACTIVE_ELEMENTS,
        );
        let elements_json = self
            .evaluate_js(&elements_js)
            .await
            .unwrap_or_else(|_| "[]".to_string());

        let elements = self.parse_elements(&elements_json);

        // Extract forms
        let forms_js = format!(
            r#"(function() {{
                    var forms = document.querySelectorAll('form');
                    var result = [];
                    for (var i = 0; i < forms.length && i < {max_forms}; i++) {{
                        var form = forms[i];
                        var fields = [];
                        var inputs = form.querySelectorAll('input, select, textarea');
                        for (var j = 0; j < inputs.length; j++) {{
                            var inp = inputs[j];
                            fields.push({{
                                name: inp.name || inp.id || '',
                                type: inp.type || 'text',
                                required: inp.required
                            }});
                        }}
                        result.push({{
                            action: form.action || null,
                            method: (form.method || 'GET').toUpperCase(),
                            fields: fields
                        }});
                    }}
                    return JSON.stringify(result);
                }})()"#,
            max_forms = MAX_FORMS,
        );
        let forms_json = self
            .evaluate_js(&forms_js)
            .await
            .unwrap_or_else(|_| "[]".to_string());

        let forms = self.parse_forms(&forms_json);

        // Extract links
        let links_js = format!(
            r#"(function() {{
                    var anchors = document.querySelectorAll('a[href]');
                    var result = [];
                    for (var i = 0; i < anchors.length && i < {max_links}; i++) {{
                        var a = anchors[i];
                        var text = (a.textContent || '').trim().substring(0, 100);
                        if (text) {{
                            result.push({{ text: text, href: a.href }});
                        }}
                    }}
                    return JSON.stringify(result);
                }})()"#,
            max_links = MAX_LINKS,
        );
        let links_json = self
            .evaluate_js(&links_js)
            .await
            .unwrap_or_else(|_| "[]".to_string());

        let links = self.parse_links(&links_json);

        // Determine page state
        let ready_state = self
            .evaluate_js("document.readyState")
            .await
            .unwrap_or_else(|_| "loading".to_string());
        let state = parse_page_state(&ready_state);

        // Clean the text content — strip surrounding quotes from JS eval results
        let text_content = strip_js_quotes(&text_content);
        let title = strip_js_quotes(&title);
        let url = strip_js_quotes(&url);

        Ok(PageSnapshot {
            url,
            title,
            text_content,
            elements,
            forms,
            links,
            state,
        })
    }

    /// Wait for the page to reach "complete" ready state.
    async fn wait_for_page_load(&self) -> Result<(), BrowserError> {
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_millis(self.page_load_timeout_ms);

        loop {
            if start.elapsed() > timeout {
                return Err(BrowserError::PageLoadTimeout(self.page_load_timeout_ms));
            }

            let ready_state = self
                .evaluate_js("document.readyState")
                .await
                .unwrap_or_default();

            if ready_state.contains("complete") || ready_state.contains("interactive") {
                return Ok(());
            }

            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }

    /// Parse interactive elements from a JSON array string.
    fn parse_elements(&mut self, json: &str) -> Vec<InteractiveElement> {
        let clean = strip_js_quotes(json);
        let items: Vec<serde_json::Value> = serde_json::from_str(&clean).unwrap_or_default();

        items
            .into_iter()
            .map(|v| {
                let id = format!("e{}", self.next_element_id);
                self.next_element_id += 1;

                InteractiveElement {
                    id,
                    tag: v["tag"].as_str().unwrap_or("unknown").to_string(),
                    role: v["role"].as_str().map(String::from),
                    text: v["text"].as_str().unwrap_or("").to_string(),
                    element_type: v["type"].as_str().map(String::from),
                    selector: v["selector"].as_str().unwrap_or("").to_string(),
                    is_visible: v["visible"].as_bool().unwrap_or(false),
                    is_enabled: v["enabled"].as_bool().unwrap_or(true),
                }
            })
            .collect()
    }

    /// Parse forms from a JSON array string.
    fn parse_forms(&self, json: &str) -> Vec<FormInfo> {
        let clean = strip_js_quotes(json);
        let items: Vec<serde_json::Value> = serde_json::from_str(&clean).unwrap_or_default();

        items
            .into_iter()
            .map(|v| FormInfo {
                action: v["action"].as_str().map(String::from),
                method: v["method"].as_str().map(String::from),
                fields: v["fields"]
                    .as_array()
                    .map(|fields| {
                        fields
                            .iter()
                            .enumerate()
                            .map(|(i, f)| FormField {
                                element_id: format!("f{}", i + 1),
                                name: f["name"].as_str().unwrap_or("").to_string(),
                                field_type: f["type"].as_str().map(String::from),
                                required: f["required"].as_bool().unwrap_or(false),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect()
    }

    /// Parse links from a JSON array string.
    fn parse_links(&self, json: &str) -> Vec<LinkInfo> {
        let clean = strip_js_quotes(json);
        let items: Vec<serde_json::Value> = serde_json::from_str(&clean).unwrap_or_default();

        items
            .into_iter()
            .map(|v| LinkInfo {
                text: v["text"].as_str().unwrap_or("").to_string(),
                href: v["href"].as_str().unwrap_or("").to_string(),
            })
            .collect()
    }
}

/// Strip surrounding double-quotes from a JavaScript eval result string.
fn strip_js_quotes(s: &str) -> String {
    let s = s.trim();
    let mut chars = s.chars();
    if chars.next() == Some('"') && s.ends_with('"') && s.len() > 2 {
        chars.next_back(); // Remove trailing quote
        chars.collect()
    } else {
        s.to_string()
    }
}

/// Parse a `document.readyState` value into a [`PageState`].
fn parse_page_state(ready_state: &str) -> PageState {
    let s = ready_state.to_lowercase();
    if s.contains("complete") {
        PageState::Complete
    } else if s.contains("interactive") {
        PageState::Interactive
    } else {
        PageState::Loading
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_js_quotes() {
        assert_eq!(strip_js_quotes(r#""hello""#), "hello");
        assert_eq!(strip_js_quotes("hello"), "hello");
        assert_eq!(strip_js_quotes(r#""""#), r#""""#); // Only 2 chars, not stripped
        assert_eq!(strip_js_quotes(""), "");
        assert_eq!(strip_js_quotes(r#""abc""#), "abc");
    }

    #[test]
    fn test_parse_page_state() {
        assert_eq!(parse_page_state("complete"), PageState::Complete);
        assert_eq!(parse_page_state("\"complete\""), PageState::Complete);
        assert_eq!(parse_page_state("interactive"), PageState::Interactive);
        assert_eq!(parse_page_state("loading"), PageState::Loading);
        assert_eq!(parse_page_state("unknown"), PageState::Loading);
    }

    #[test]
    fn test_parse_elements() {
        let devtools = Arc::new(DevToolsClient::new(0));
        let mut session = BrowserSession::new("test".to_string(), devtools, 30_000);

        let json = r##"[
            {"tag":"button","role":null,"text":"Submit","type":null,"selector":"#submit","visible":true,"enabled":true},
            {"tag":"input","role":null,"text":"","type":"text","selector":"[name=\"email\"]","visible":true,"enabled":true}
        ]"##;

        let elements = session.parse_elements(json);
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0].id, "e1");
        assert_eq!(elements[0].tag, "button");
        assert_eq!(elements[0].text, "Submit");
        assert!(elements[0].is_visible);
        assert_eq!(elements[1].id, "e2");
        assert_eq!(elements[1].tag, "input");
        assert_eq!(elements[1].element_type.as_deref(), Some("text"));
    }

    #[test]
    fn test_parse_forms() {
        let devtools = Arc::new(DevToolsClient::new(0));
        let session = BrowserSession::new("test".to_string(), devtools, 30_000);

        let json = r#"[{
            "action": "/login",
            "method": "POST",
            "fields": [
                {"name":"username","type":"text","required":true},
                {"name":"password","type":"password","required":true}
            ]
        }]"#;

        let forms = session.parse_forms(json);
        assert_eq!(forms.len(), 1);
        assert_eq!(forms[0].action.as_deref(), Some("/login"));
        assert_eq!(forms[0].fields.len(), 2);
        assert!(forms[0].fields[0].required);
    }

    #[test]
    fn test_parse_links() {
        let devtools = Arc::new(DevToolsClient::new(0));
        let session = BrowserSession::new("test".to_string(), devtools, 30_000);

        let json = r#"[
            {"text":"Home","href":"https://example.com"},
            {"text":"About","href":"https://example.com/about"}
        ]"#;

        let links = session.parse_links(json);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].text, "Home");
        assert_eq!(links[1].href, "https://example.com/about");
    }
}
