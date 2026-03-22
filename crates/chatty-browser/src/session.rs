//! Browser session — high-level operations built on [`BrowserBackend`].
//!
//! All DOM interaction is performed via JavaScript snippets executed through
//! `backend.evaluate_js()`. This keeps the backend trait thin and makes the
//! session code engine-agnostic.

use std::sync::Arc;

use crate::backend::{BrowserBackend, Cookie, TabId};
use crate::page::{FormField, FormInfo, InteractiveElement, LinkInfo, LoginHint, PageSnapshot};

/// Maximum characters for `text_content` in a page snapshot.
const MAX_TEXT_CONTENT_LEN: usize = 3_000;
/// Maximum number of interactive elements to include.
const MAX_ELEMENTS: usize = 50;
/// Maximum number of links to include.
const MAX_LINKS: usize = 50;
/// Default page load timeout in milliseconds.
const DEFAULT_LOAD_TIMEOUT_MS: u64 = 15_000;
/// Redaction sentinel for password fields.
pub const PASSWORD_REDACTED: &str = "●●●●";

/// Shared cookie jar for HTTP-based authentication.
///
/// When the browser backend is unavailable (WryBackend stub), the
/// `browser_auth` tool authenticates via HTTP and stores session cookies
/// here. The `browse` tool's HTTP fallback reads cookies from the same jar
/// so subsequent page fetches are authenticated.
pub type SharedCookieJar = Arc<reqwest::cookie::Jar>;

/// Escape a string for safe embedding in a JavaScript string literal.
///
/// Handles: backslashes, quotes, newlines, carriage returns, tabs, and
/// other control characters that could break a JS string or enable injection.
pub fn escape_js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0C' => out.push_str("\\f"),
            c if c.is_control() => {
                // Escape other control chars as \uXXXX
                for unit in c.encode_utf16(&mut [0; 2]) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// Truncate a string to approximately `max_len` characters at a word boundary,
/// respecting UTF-8 boundaries. Appends '…' if truncated.
fn truncate_text(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_string();
    }
    // Find a safe char boundary at or before max_len
    let safe_end = text
        .char_indices()
        .take_while(|(i, _)| *i <= max_len)
        .last()
        .map(|(i, _)| i)
        .unwrap_or(0);
    // Try to break at a word boundary
    let truncation_point = text[..safe_end].rfind(' ').unwrap_or(safe_end);
    let mut result = text[..truncation_point].to_string();
    result.push('…');
    result
}

/// High-level browser session wrapping a [`BrowserBackend`].
///
/// Provides navigate, click, fill, extract, and snapshot operations by
/// building JavaScript snippets and evaluating them via the backend.
///
/// Also carries a [`SharedCookieJar`] for HTTP-based fallback authentication.
pub struct BrowserSession {
    backend: Arc<dyn BrowserBackend>,
    /// Cookie jar shared between the auth tool and browse tool's HTTP fallback.
    cookie_jar: SharedCookieJar,
}

impl BrowserSession {
    pub fn new(backend: Arc<dyn BrowserBackend>) -> Self {
        Self {
            backend,
            cookie_jar: Arc::new(reqwest::cookie::Jar::default()),
        }
    }

    /// Get a reference to the underlying backend.
    pub fn backend(&self) -> &Arc<dyn BrowserBackend> {
        &self.backend
    }

    /// Get a reference to the shared cookie jar used by HTTP fallbacks.
    pub fn cookie_jar(&self) -> &SharedCookieJar {
        &self.cookie_jar
    }

    /// Navigate a tab to `url`, wait for load, and return a [`PageSnapshot`].
    pub async fn navigate_and_snapshot(
        &self,
        tab: &TabId,
        url: &str,
        login_profiles: &[crate::credential::types::LoginProfile],
    ) -> anyhow::Result<PageSnapshot> {
        self.backend.navigate(tab, url).await?;
        self.backend
            .wait_for_load(tab, DEFAULT_LOAD_TIMEOUT_MS)
            .await?;
        self.try_dismiss_cookie_consent(tab).await;
        self.build_page_snapshot(tab, login_profiles).await
    }

    /// Build a [`PageSnapshot`] from the current state of a tab.
    pub async fn build_page_snapshot(
        &self,
        tab: &TabId,
        login_profiles: &[crate::credential::types::LoginProfile],
    ) -> anyhow::Result<PageSnapshot> {
        let js = Self::snapshot_js();
        let raw = self.backend.evaluate_js(tab, &js).await?;
        let data: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("Failed to parse snapshot JS result: {e}"))?;

        let url = data["url"].as_str().unwrap_or_default().to_string();
        let title = data["title"].as_str().unwrap_or_default().to_string();

        let text_content = truncate_text(
            data["textContent"].as_str().unwrap_or_default(),
            MAX_TEXT_CONTENT_LEN,
        );

        // Parse interactive elements
        let elements = Self::parse_elements(&data["elements"]);

        // Parse forms
        let forms = Self::parse_forms(&data["forms"]);

        // Parse links
        let links = Self::parse_links(&data["links"]);

        // Auto-detect login hint
        let has_password_field = elements
            .iter()
            .any(|e| e.input_type.as_deref() == Some("password"));
        let login_hint = if has_password_field {
            login_profiles
                .iter()
                .find(|p| url.contains(&p.url_pattern) || p.url_pattern.contains(&url))
                .map(|p| LoginHint {
                    credential_name: p.name.clone(),
                    message: format!(
                        "Login form detected. Use browser_auth with credential \"{}\".",
                        p.name
                    ),
                })
        } else {
            None
        };

        // OG image / description
        let og_image_url = data["ogImageUrl"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(String::from);
        let description = data["description"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(String::from);

        Ok(PageSnapshot {
            url,
            title,
            text_content,
            elements,
            forms,
            links,
            login_hint,
            og_image_url,
            description,
            screenshot_path: None,
        })
    }

    // ── Cookie consent auto-dismiss ─────────────────────────────────────

    /// Best-effort attempt to dismiss cookie consent banners.
    ///
    /// Polls up to 3 seconds for a consent banner to appear, then clicks
    /// the "accept all" button. Covers popular CMPs (Cookiebot, OneTrust,
    /// Usercentrics, Quantcast, Didomi, etc.) and generic text-matching.
    async fn try_dismiss_cookie_consent(&self, tab: &TabId) {
        // The JS returns { dismissed: true/false } — we retry a few times
        // because banners often animate in after page load.
        let js = Self::cookie_dismiss_js();

        for attempt in 0..6 {
            // Wait before each attempt (banners load lazily)
            if attempt == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            } else {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }

            match self.backend.evaluate_js(tab, &js).await {
                Ok(result) => {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&result) {
                        if json["dismissed"].as_bool() == Some(true) {
                            tracing::info!(
                                result = %result,
                                attempt = attempt,
                                "Auto-dismissed cookie consent banner"
                            );
                            // Wait for dismiss animation
                            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                            return;
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!(error = ?e, "Cookie consent dismiss JS failed (non-fatal)");
                    return;
                }
            }
        }
        tracing::debug!("No cookie consent banner found after retries");
    }

    /// JavaScript that finds and clicks common cookie consent "accept" buttons.
    fn cookie_dismiss_js() -> String {
        r#"(() => {
            // Helper: check if element is visible (works for position:fixed too)
            function isVisible(el) {
                if (!el) return false;
                const style = window.getComputedStyle(el);
                return style.display !== 'none'
                    && style.visibility !== 'hidden'
                    && style.opacity !== '0'
                    && el.getBoundingClientRect().height > 0;
            }

            // 1. Try specific CMP selectors (most reliable)
            const selectors = [
                // Usercentrics (Komoot, etc.)
                '[data-testid="uc-accept-all-button"]',
                '#uc-btn-accept-banner',
                '.uc-accept-all-button',
                'button[data-testid="uc-accept-all-button"]',
                // Cookiebot
                '#CybotCookiebotDialogBodyLevelButtonLevelOptinAllowAll',
                '#CybotCookiebotDialogBodyButtonAccept',
                // OneTrust
                '#onetrust-accept-btn-handler',
                '.onetrust-close-btn-handler',
                // Cookie Consent (Osano)
                '.cc-accept', '.cc-btn.cc-allow',
                '.cc-compliance .cc-btn:first-child',
                // Quantcast
                '.qc-cmp2-summary-buttons button:first-child',
                // Didomi
                '#didomi-notice-agree-button',
                // Funding Choices (Google)
                '.fc-cta-consent', '.fc-button.fc-cta-consent',
                // Generic IDs/classes
                '#accept-cookies', '#cookie-accept', '#cookies-accept',
                '#acceptAll', '#accept-all', '#cookieAcceptAll',
                '.cookie-accept', '.cookie-consent-accept',
                '.js-cookie-accept', '.js-accept-cookies',
                '[data-action="accept-cookies"]',
                '[data-consent="accept"]',
                'button[mode="primary"][data-gdpr="accept"]',
                // GDPR generic
                '.gdpr-accept', '#gdpr-accept',
                '.consent-accept', '#consent-accept',
            ];
            for (const sel of selectors) {
                const el = document.querySelector(sel);
                if (isVisible(el)) {
                    el.click();
                    return JSON.stringify({ dismissed: true, method: "selector", selector: sel });
                }
            }

            // 2. Search inside shadow DOMs of known CMP containers
            const shadowHosts = document.querySelectorAll(
                '#usercentrics-root, div[id*="cookie"], div[id*="consent"]'
            );
            for (const host of shadowHosts) {
                if (host.shadowRoot) {
                    const btns = host.shadowRoot.querySelectorAll('button');
                    for (const btn of btns) {
                        const text = (btn.innerText || '').trim().toLowerCase();
                        if (text.includes('accept') || text.includes('akzeptieren')
                            || text.includes('allow all') || text.includes('alle')) {
                            if (isVisible(btn)) {
                                btn.click();
                                return JSON.stringify({ dismissed: true, method: "shadow", text: text });
                            }
                        }
                    }
                }
            }

            // 3. Fallback: find buttons by text content (multilingual)
            const acceptTexts = [
                'accept all', 'accept cookies', 'accept all cookies',
                'accept & close', 'allow all', 'allow all cookies',
                'allow cookies', 'agree', 'agree to all', 'i agree',
                'got it', 'ok, got it', 'okay',
                'alle akzeptieren', 'akzeptieren', 'alles akzeptieren',
                'zustimmen', 'alle zulassen',
                'accepter tout', 'tout accepter', 'accepter',
                'j\'accepte', 'autoriser',
                'accepteer alles', 'alles accepteren', 'akkoord',
                'aceptar todo', 'aceptar todas', 'aceptar',
            ];
            const buttons = document.querySelectorAll(
                'button, a[role="button"], [role="button"], input[type="submit"], input[type="button"]'
            );
            for (const btn of buttons) {
                const text = (btn.innerText || btn.textContent || '').trim().toLowerCase();
                if (text && acceptTexts.some(t => text === t || text.startsWith(t + ' '))) {
                    if (isVisible(btn)) {
                        btn.click();
                        return JSON.stringify({ dismissed: true, method: "text", text: text });
                    }
                }
            }

            return JSON.stringify({ dismissed: false });
        })()"#
            .to_string()
    }

    // ── Element interaction ──────────────────────────────────────────────

    /// Click an interactive element by its stable ID (e.g., "e1").
    pub async fn click_element(&self, tab: &TabId, element_id: &str) -> anyhow::Result<String> {
        let js = format!(
            r#"(() => {{
                const els = document.querySelectorAll(
                    'a, button, input, select, textarea, [role="button"], [onclick]'
                );
                const idx = parseInt("{id}".replace("e", ""), 10) - 1;
                if (idx < 0 || idx >= els.length) return JSON.stringify({{ error: "Element {id} not found" }});
                const el = els[idx];
                el.click();
                return JSON.stringify({{ success: true, tag: el.tagName.toLowerCase() }});
            }})()"#,
            id = element_id
        );
        self.backend.evaluate_js(tab, &js).await
    }

    /// Fill a form field by its stable ID with a value.
    pub async fn fill_element(
        &self,
        tab: &TabId,
        element_id: &str,
        value: &str,
    ) -> anyhow::Result<String> {
        let escaped = escape_js_string(value);
        let js = format!(
            r#"(() => {{
                const els = document.querySelectorAll(
                    'a, button, input, select, textarea, [role="button"], [onclick]'
                );
                const idx = parseInt("{id}".replace("e", ""), 10) - 1;
                if (idx < 0 || idx >= els.length) return JSON.stringify({{ error: "Element {id} not found" }});
                const el = els[idx];
                const nativeInputValueSetter = Object.getOwnPropertyDescriptor(
                    window.HTMLInputElement.prototype, 'value'
                )?.set || Object.getOwnPropertyDescriptor(
                    window.HTMLTextAreaElement.prototype, 'value'
                )?.set;
                if (nativeInputValueSetter) {{
                    nativeInputValueSetter.call(el, "{val}");
                }} else {{
                    el.value = "{val}";
                }}
                el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                return JSON.stringify({{ success: true, tag: el.tagName.toLowerCase() }});
            }})()"#,
            id = element_id,
            val = escaped
        );
        self.backend.evaluate_js(tab, &js).await
    }

    /// Select an option in a `<select>` element by its stable ID.
    pub async fn select_option(
        &self,
        tab: &TabId,
        element_id: &str,
        option_value: &str,
    ) -> anyhow::Result<String> {
        let escaped = escape_js_string(option_value);
        let js = format!(
            r#"(() => {{
                const els = document.querySelectorAll(
                    'a, button, input, select, textarea, [role="button"], [onclick]'
                );
                const idx = parseInt("{id}".replace("e", ""), 10) - 1;
                if (idx < 0 || idx >= els.length) return JSON.stringify({{ error: "Element {id} not found" }});
                const el = els[idx];
                if (el.tagName !== 'SELECT') return JSON.stringify({{ error: "Element {id} is not a select" }});
                el.value = "{val}";
                el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                return JSON.stringify({{ success: true }});
            }})()"#,
            id = element_id,
            val = escaped
        );
        self.backend.evaluate_js(tab, &js).await
    }

    /// Scroll the page by a given number of pixels (positive = down).
    pub async fn scroll(&self, tab: &TabId, pixels: i32) -> anyhow::Result<String> {
        let js = format!(
            r#"(() => {{
                window.scrollBy(0, {pixels});
                return JSON.stringify({{
                    success: true,
                    scrollY: window.scrollY,
                    scrollHeight: document.body.scrollHeight
                }});
            }})()"#,
            pixels = pixels
        );
        self.backend.evaluate_js(tab, &js).await
    }

    /// Wait for a CSS selector to appear, with a timeout in milliseconds.
    pub async fn wait_for_selector(
        &self,
        tab: &TabId,
        selector: &str,
        timeout_ms: u64,
    ) -> anyhow::Result<String> {
        let escaped = escape_js_string(selector);
        let js = format!(
            r#"(async () => {{
                const deadline = Date.now() + {timeout};
                while (Date.now() < deadline) {{
                    if (document.querySelector("{sel}")) {{
                        return JSON.stringify({{ found: true }});
                    }}
                    await new Promise(r => setTimeout(r, 100));
                }}
                return JSON.stringify({{ found: false, error: "Timeout waiting for selector: {sel}" }});
            }})()"#,
            timeout = timeout_ms,
            sel = escaped
        );
        self.backend.evaluate_js(tab, &js).await
    }

    // ── Extraction helpers ───────────────────────────────────────────────

    /// Extract all visible text from the current page.
    pub async fn extract_text(&self, tab: &TabId) -> anyhow::Result<String> {
        let js = r#"(() => {
            const clone = document.body.cloneNode(true);
            clone.querySelectorAll('script, style, noscript, svg').forEach(el => el.remove());
            return JSON.stringify({ text: clone.innerText || clone.textContent || "" });
        })()"#;
        let raw = self.backend.evaluate_js(tab, js).await?;
        let data: serde_json::Value = serde_json::from_str(&raw)?;
        Ok(data["text"].as_str().unwrap_or_default().to_string())
    }

    /// Extract all links from the current page.
    pub async fn extract_links(&self, tab: &TabId) -> anyhow::Result<Vec<LinkInfo>> {
        let js = r#"(() => {
            const links = Array.from(document.querySelectorAll('a[href]')).slice(0, 100).map(a => ({
                text: (a.innerText || a.textContent || "").trim().substring(0, 100),
                href: a.href
            }));
            return JSON.stringify(links);
        })()"#;
        let raw = self.backend.evaluate_js(tab, js).await?;
        let links: Vec<LinkInfo> = serde_json::from_str(&raw)?;
        Ok(links)
    }

    /// Extract tables from the current page as arrays of rows.
    pub async fn extract_tables(&self, tab: &TabId) -> anyhow::Result<Vec<Vec<Vec<String>>>> {
        let js = r#"(() => {
            const tables = Array.from(document.querySelectorAll('table')).slice(0, 10).map(table => {
                return Array.from(table.querySelectorAll('tr')).slice(0, 50).map(tr => {
                    return Array.from(tr.querySelectorAll('th, td')).map(cell =>
                        (cell.innerText || cell.textContent || "").trim().substring(0, 200)
                    );
                });
            });
            return JSON.stringify(tables);
        })()"#;
        let raw = self.backend.evaluate_js(tab, js).await?;
        let tables: Vec<Vec<Vec<String>>> = serde_json::from_str(&raw)?;
        Ok(tables)
    }

    // ── Cookie helpers ───────────────────────────────────────────────────

    /// Get all cookies for the current tab.
    pub async fn get_cookies(&self, tab: &TabId) -> anyhow::Result<Vec<Cookie>> {
        self.backend.get_cookies(tab).await
    }

    /// Inject cookies into a tab.
    pub async fn set_cookies(&self, tab: &TabId, cookies: &[Cookie]) -> anyhow::Result<()> {
        self.backend.set_cookies(tab, cookies).await
    }

    // ── Private helpers ──────────────────────────────────────────────────

    /// JavaScript snippet that extracts a full page snapshot.
    fn snapshot_js() -> String {
        r#"(() => {
            // Text content
            const clone = document.body.cloneNode(true);
            clone.querySelectorAll('script, style, noscript, svg').forEach(el => el.remove());
            const textContent = (clone.innerText || clone.textContent || "").trim();

            // Interactive elements
            const interactiveSelector = 'a, button, input, select, textarea, [role="button"], [onclick]';
            const interactiveEls = Array.from(document.querySelectorAll(interactiveSelector)).slice(0, 50);
            const elements = interactiveEls.map((el, i) => {
                const tag = el.tagName.toLowerCase();
                const rect = el.getBoundingClientRect();
                const visible = rect.width > 0 && rect.height > 0;
                const label = (el.innerText || el.textContent || el.getAttribute('aria-label') || el.getAttribute('placeholder') || el.name || "").trim().substring(0, 100);
                const result = {
                    id: "e" + (i + 1),
                    element_type: tag,
                    label: label,
                    visible: visible,
                    disabled: el.disabled || false
                };
                if (el.value !== undefined && el.value !== "") {
                    result.value = el.type === "password" ? "●●●●" : el.value.substring(0, 100);
                }
                if (el.name) result.name = el.name;
                if (el.type) result.input_type = el.type;
                if (el.href) result.href = el.href;
                return result;
            });

            // Forms
            const forms = Array.from(document.querySelectorAll('form')).slice(0, 10).map(form => {
                const fields = Array.from(form.querySelectorAll('input, select, textarea')).map(f => ({
                    name: f.name || "",
                    field_type: f.type || f.tagName.toLowerCase(),
                    value: f.type === "password" ? (f.value ? "●●●●" : null) : (f.value || null),
                    required: f.required || false,
                    placeholder: f.placeholder || null
                }));
                return {
                    id: form.id || null,
                    name: form.name || null,
                    action: form.action || "",
                    method: (form.method || "GET").toUpperCase(),
                    fields: fields
                };
            });

            // Links
            const links = Array.from(document.querySelectorAll('a[href]')).slice(0, 50).map(a => ({
                text: (a.innerText || a.textContent || "").trim().substring(0, 100),
                href: a.href
            }));

            // OG image
            const ogImage = document.querySelector('meta[property="og:image"]');
            const ogImageUrl = ogImage ? ogImage.content : "";

            // Description
            const metaDesc = document.querySelector('meta[name="description"]') ||
                             document.querySelector('meta[property="og:description"]');
            const description = metaDesc ? metaDesc.content : "";

            return JSON.stringify({
                url: window.location.href,
                title: document.title,
                textContent: textContent,
                elements: elements,
                forms: forms,
                links: links,
                ogImageUrl: ogImageUrl,
                description: description
            });
        })()"#
        .to_string()
    }

    fn parse_elements(value: &serde_json::Value) -> Vec<InteractiveElement> {
        let Some(arr) = value.as_array() else {
            return Vec::new();
        };
        arr.iter()
            .take(MAX_ELEMENTS)
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect()
    }

    fn parse_forms(value: &serde_json::Value) -> Vec<FormInfo> {
        let Some(arr) = value.as_array() else {
            return Vec::new();
        };
        arr.iter()
            .map(|v| {
                let fields: Vec<FormField> = v["fields"]
                    .as_array()
                    .map(|fa| {
                        fa.iter()
                            .filter_map(|fv| serde_json::from_value(fv.clone()).ok())
                            .collect()
                    })
                    .unwrap_or_default();
                FormInfo {
                    id: v["id"].as_str().map(String::from),
                    name: v["name"].as_str().map(String::from),
                    action: v["action"].as_str().unwrap_or_default().to_string(),
                    method: v["method"].as_str().unwrap_or("GET").to_string(),
                    fields,
                }
            })
            .collect()
    }

    fn parse_links(value: &serde_json::Value) -> Vec<LinkInfo> {
        let Some(arr) = value.as_array() else {
            return Vec::new();
        };
        arr.iter()
            .take(MAX_LINKS)
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_js_is_valid_string() {
        let js = BrowserSession::snapshot_js();
        assert!(js.contains("document.body.cloneNode"));
        assert!(js.contains("interactiveSelector"));
        assert!(js.contains("JSON.stringify"));
    }

    #[test]
    fn test_parse_elements_empty() {
        let val = serde_json::json!([]);
        let elements = BrowserSession::parse_elements(&val);
        assert!(elements.is_empty());
    }

    #[test]
    fn test_parse_elements_with_data() {
        let val = serde_json::json!([
            {
                "id": "e1",
                "element_type": "button",
                "label": "Submit",
                "visible": true,
                "disabled": false
            },
            {
                "id": "e2",
                "element_type": "input",
                "label": "",
                "name": "email",
                "input_type": "email",
                "visible": true,
                "disabled": false
            }
        ]);
        let elements = BrowserSession::parse_elements(&val);
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0].id, "e1");
        assert_eq!(elements[0].element_type, "button");
        assert_eq!(elements[1].name.as_deref(), Some("email"));
    }

    #[test]
    fn test_parse_forms_with_fields() {
        let val = serde_json::json!([
            {
                "id": "login-form",
                "name": "login",
                "action": "/auth/login",
                "method": "POST",
                "fields": [
                    {
                        "name": "username",
                        "field_type": "text",
                        "required": true
                    },
                    {
                        "name": "password",
                        "field_type": "password",
                        "value": "●●●●",
                        "required": true
                    }
                ]
            }
        ]);
        let forms = BrowserSession::parse_forms(&val);
        assert_eq!(forms.len(), 1);
        assert_eq!(forms[0].name.as_deref(), Some("login"));
        assert_eq!(forms[0].fields.len(), 2);
        assert_eq!(forms[0].fields[1].field_type, "password");
    }

    #[test]
    fn test_parse_links() {
        let val = serde_json::json!([
            { "text": "Home", "href": "https://example.com/" },
            { "text": "About", "href": "https://example.com/about" }
        ]);
        let links = BrowserSession::parse_links(&val);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].text, "Home");
    }

    #[test]
    fn test_escape_js_string_basic() {
        assert_eq!(escape_js_string("hello"), "hello");
        assert_eq!(escape_js_string(r#"he"llo"#), r#"he\"llo"#);
        assert_eq!(escape_js_string("he\\llo"), "he\\\\llo");
    }

    #[test]
    fn test_escape_js_string_special_chars() {
        assert_eq!(escape_js_string("line1\nline2"), "line1\\nline2");
        assert_eq!(escape_js_string("tab\there"), "tab\\there");
        assert_eq!(escape_js_string("cr\rhere"), "cr\\rhere");
        assert_eq!(escape_js_string("it's"), "it\\'s");
    }

    #[test]
    fn test_escape_js_string_control_chars() {
        let input = "hello\x00world";
        let escaped = escape_js_string(input);
        assert!(escaped.contains("\\u0000"));
    }

    #[test]
    fn test_truncate_text_short() {
        let text = "Hello, world!";
        assert_eq!(truncate_text(text, 100), "Hello, world!");
    }

    #[test]
    fn test_truncate_text_long() {
        let text = "word1 word2 word3 word4 word5";
        let truncated = truncate_text(text, 15);
        assert!(truncated.ends_with('…'));
        assert!(truncated.len() < 20);
    }

    #[test]
    fn test_truncate_text_multibyte() {
        // This should not panic even with multi-byte chars
        let text = "こんにちは世界 Hello 你好世界";
        let truncated = truncate_text(text, 10);
        assert!(truncated.ends_with('…'));
    }
}
