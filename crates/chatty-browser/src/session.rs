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
    /// Mock mode: return fake page data without a real browser.
    mock_mode: bool,
}

impl BrowserSession {
    /// Create a new session.
    pub(crate) fn new(
        id: String,
        devtools: Arc<DevToolsClient>,
        page_load_timeout_ms: u64,
        mock_mode: bool,
    ) -> Self {
        Self {
            id,
            devtools,
            actor_id: "tab1".to_string(), // Default actor; updated on first navigation
            current_url: None,
            page_load_timeout_ms,
            next_element_id: 1,
            mock_mode,
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

        if self.mock_mode {
            return Ok(self.build_mock_snapshot(url));
        }

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

    /// Get the current page URL (resolved from JS `window.location.href`).
    pub async fn current_url_from_js(&self) -> Result<String, BrowserError> {
        let raw = self.evaluate_js("window.location.href").await?;
        Ok(strip_js_quotes(&raw))
    }

    /// Extract the domain from the current page URL.
    pub async fn current_url_domain(&self) -> Result<String, BrowserError> {
        let raw = self.evaluate_js("window.location.hostname").await?;
        Ok(strip_js_quotes(&raw))
    }

    /// Get all cookies for the current page via `document.cookie`.
    ///
    /// Returns a list of (name, value) pairs. Only cookies visible to JS
    /// are returned (HttpOnly cookies are excluded by browsers).
    pub async fn get_cookies(&self) -> Result<Vec<(String, String)>, BrowserError> {
        if self.mock_mode {
            return Ok(vec![
                ("mock_session".to_string(), "test_value".to_string()),
            ]);
        }

        let raw = self.evaluate_js("document.cookie").await?;
        let cookie_str = strip_js_quotes(&raw);

        let cookies: Vec<(String, String)> = cookie_str
            .split(';')
            .filter_map(|pair| {
                let pair = pair.trim();
                if pair.is_empty() {
                    return None;
                }
                let mut parts = pair.splitn(2, '=');
                let name = parts.next()?.trim().to_string();
                let value = parts.next().unwrap_or("").trim().to_string();
                if name.is_empty() {
                    return None;
                }
                Some((name, value))
            })
            .collect();

        Ok(cookies)
    }

    /// Set cookies on the current page via `document.cookie`.
    ///
    /// Each (name, value, domain, path) tuple is set individually.
    /// Note: HttpOnly cookies cannot be set via JS — this only sets
    /// cookies visible to JavaScript.
    pub async fn set_cookies(
        &self,
        cookies: &[(String, String, String, String)],
    ) -> Result<(), BrowserError> {
        if self.mock_mode {
            return Ok(());
        }

        for (name, value, domain, path) in cookies {
            let js = format!(
                "document.cookie = '{}={}; domain={}; path={}; SameSite=Lax'",
                escape_cookie_value(name),
                escape_cookie_value(value),
                escape_cookie_value(domain),
                escape_cookie_value(path),
            );
            self.evaluate_js(&js).await?;
        }

        Ok(())
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

        // Extract Open Graph metadata for visual preview
        let og_image_url = self
            .evaluate_js(
                r#"(function() {
                    var el = document.querySelector('meta[property="og:image"]');
                    return el ? el.getAttribute('content') : '';
                })()"#,
            )
            .await
            .ok()
            .map(|s| strip_js_quotes(&s))
            .filter(|s| !s.is_empty());

        let description = self
            .evaluate_js(
                r#"(function() {
                    var el = document.querySelector('meta[property="og:description"]')
                          || document.querySelector('meta[name="description"]');
                    return el ? el.getAttribute('content') : '';
                })()"#,
            )
            .await
            .ok()
            .map(|s| strip_js_quotes(&s))
            .filter(|s| !s.is_empty());

        Ok(PageSnapshot {
            url,
            title,
            text_content,
            elements,
            forms,
            links,
            state,
            og_image_url,
            description,
            raw_html: None,
        })
    }

    /// Build a realistic mock page snapshot for testing without a real browser.
    ///
    /// Generates a plausible `PageSnapshot` based on the URL domain, including
    /// sample text content, interactive elements, forms (for login-like pages),
    /// and links. This allows testing the full browse tool pipeline end-to-end.
    fn build_mock_snapshot(&mut self, url: &str) -> PageSnapshot {
        self.next_element_id = 1;
        self.current_url = Some(url.to_string());

        // Extract domain for realistic mock content
        let domain = url
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or("example.com");

        let title = format!("Welcome to {} - Mock Page", domain);
        let text_content = format!(
            "This is a mock page snapshot for {}.\n\n\
             The browse tool is running in mock mode, which means it returns \
             realistic fake data without launching a real browser engine. \
             This is useful for testing LLM tool integration.\n\n\
             In production, the browse tool launches a Verso (Servo-based) browser \
             that executes JavaScript, renders the page, and extracts structured data.\n\n\
             Page content from {} would appear here with the full text, \
             interactive elements, forms, and links extracted from the DOM.",
            url, domain
        );

        let elements = vec![
            InteractiveElement {
                id: self.next_element_id_str(),
                tag: "input".to_string(),
                role: None,
                text: String::new(),
                element_type: Some("text".to_string()),
                selector: "#search-input".to_string(),
                is_visible: true,
                is_enabled: true,
            },
            InteractiveElement {
                id: self.next_element_id_str(),
                tag: "button".to_string(),
                role: Some("button".to_string()),
                text: "Search".to_string(),
                element_type: Some("submit".to_string()),
                selector: "#search-button".to_string(),
                is_visible: true,
                is_enabled: true,
            },
            InteractiveElement {
                id: self.next_element_id_str(),
                tag: "a".to_string(),
                role: Some("link".to_string()),
                text: "Sign In".to_string(),
                element_type: None,
                selector: "#signin-link".to_string(),
                is_visible: true,
                is_enabled: true,
            },
        ];

        let forms = vec![FormInfo {
            action: Some(format!("https://{}/search", domain)),
            method: Some("GET".to_string()),
            fields: vec![FormField {
                element_id: "e1".to_string(),
                name: "q".to_string(),
                field_type: Some("text".to_string()),
                required: false,
            }],
        }];

        let links = vec![
            LinkInfo {
                text: "Home".to_string(),
                href: format!("https://{}/", domain),
            },
            LinkInfo {
                text: "About".to_string(),
                href: format!("https://{}/about", domain),
            },
            LinkInfo {
                text: "Contact".to_string(),
                href: format!("https://{}/contact", domain),
            },
        ];

        debug!(url, "Built mock page snapshot");

        PageSnapshot {
            url: url.to_string(),
            title,
            text_content,
            elements,
            forms,
            links,
            state: PageState::Complete,
            og_image_url: None,
            description: Some(format!(
                "Mock page snapshot for {} — browse tool running in test mode.",
                domain
            )),
            raw_html: None,
        }
    }

    /// Generate the next element ID string (e.g., "e1", "e2").
    fn next_element_id_str(&mut self) -> String {
        let id = format!("e{}", self.next_element_id);
        self.next_element_id += 1;
        id
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
    if chars.next() == Some('"') && s.ends_with('"') && s.len() >= 2 {
        chars.next_back(); // Remove trailing quote
        chars.collect()
    } else {
        s.to_string()
    }
}

/// Escape a cookie name or value for safe inclusion in a `document.cookie` assignment.
/// Escapes characters that could break the JS string literal or cookie parsing.
fn escape_cookie_value(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\'' => result.push_str("%27"),
            ';' => result.push_str("%3B"),
            '\\' => result.push_str("%5C"),
            '"' => result.push_str("%22"),
            '\n' => result.push_str("%0A"),
            '\r' => result.push_str("%0D"),
            '=' => result.push_str("%3D"),
            ' ' => result.push_str("%20"),
            _ => result.push(c),
        }
    }
    result
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
        assert_eq!(strip_js_quotes(r#""""#), ""); // JS empty string
        assert_eq!(strip_js_quotes(""), "");
        assert_eq!(strip_js_quotes(r#""abc""#), "abc");
    }

    #[test]
    fn test_escape_cookie_value() {
        assert_eq!(escape_cookie_value("simple"), "simple");
        assert_eq!(escape_cookie_value("a;b"), "a%3Bb");
        assert_eq!(escape_cookie_value("a'b"), "a%27b");
        assert_eq!(escape_cookie_value("a\\b"), "a%5Cb");
        assert_eq!(escape_cookie_value("a\"b"), "a%22b");
        assert_eq!(escape_cookie_value("a\nb"), "a%0Ab");
        assert_eq!(escape_cookie_value("a\rb"), "a%0Db");
        assert_eq!(escape_cookie_value("a=b"), "a%3Db");
        assert_eq!(escape_cookie_value("a b"), "a%20b");
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
        let mut session = BrowserSession::new("test".to_string(), devtools, 30_000, false);

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
        let session = BrowserSession::new("test".to_string(), devtools, 30_000, false);

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
        let session = BrowserSession::new("test".to_string(), devtools, 30_000, false);

        let json = r#"[
            {"text":"Home","href":"https://example.com"},
            {"text":"About","href":"https://example.com/about"}
        ]"#;

        let links = session.parse_links(json);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].text, "Home");
        assert_eq!(links[1].href, "https://example.com/about");
    }

    #[tokio::test]
    async fn test_mock_session_navigate() {
        let devtools = Arc::new(DevToolsClient::new(0));
        let mut session = BrowserSession::new("mock-test".to_string(), devtools, 30_000, true);

        let snapshot = session
            .navigate("https://example.com/test")
            .await
            .expect("Mock navigation should succeed");

        assert_eq!(snapshot.url, "https://example.com/test");
        assert!(snapshot.title.contains("example.com"));
        assert!(!snapshot.text_content.is_empty());
        assert_eq!(snapshot.state, PageState::Complete);
        // Should have mock interactive elements
        assert!(!snapshot.elements.is_empty());
        assert_eq!(snapshot.elements[0].id, "e1");
        // Should have mock forms
        assert!(!snapshot.forms.is_empty());
        // Should have mock links
        assert!(!snapshot.links.is_empty());
    }

    #[tokio::test]
    async fn test_mock_session_navigate_different_urls() {
        let devtools = Arc::new(DevToolsClient::new(0));
        let mut session = BrowserSession::new("mock-test".to_string(), devtools, 30_000, true);

        let snap1 = session
            .navigate("https://github.com")
            .await
            .expect("Mock navigation should succeed");
        assert!(snap1.title.contains("github.com"));

        let snap2 = session
            .navigate("https://rust-lang.org/docs")
            .await
            .expect("Mock navigation should succeed");
        assert!(snap2.title.contains("rust-lang.org"));
        assert_eq!(snap2.url, "https://rust-lang.org/docs");
    }
}
