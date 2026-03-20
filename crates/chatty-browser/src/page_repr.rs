use serde::{Deserialize, Serialize};

/// Snapshot of a web page suitable for LLM consumption.
///
/// The agent receives this structured representation instead of raw HTML,
/// allowing it to reason about page content and interactive elements.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PageSnapshot {
    /// Current page URL.
    pub url: String,
    /// Page title from `<title>` element.
    pub title: String,
    /// Simplified, readable text content extracted from the page.
    pub text_content: String,
    /// Interactive elements with stable session-scoped IDs.
    pub elements: Vec<InteractiveElement>,
    /// Detected forms with their fields.
    pub forms: Vec<FormInfo>,
    /// Links found on the page.
    pub links: Vec<LinkInfo>,
    /// Current page loading state.
    pub state: PageState,
}

impl PageSnapshot {
    /// Render the snapshot as a compact text representation for the LLM context.
    pub fn to_llm_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("Page: {} ({})\n", self.title, self.url));
        out.push_str(&format!("State: {:?}\n\n", self.state));

        if !self.text_content.is_empty() {
            // Truncate very long text to keep within reasonable token budgets
            let max_text = 4000;
            if self.text_content.len() > max_text {
                out.push_str(&self.text_content[..max_text]);
                out.push_str("\n... (truncated)\n");
            } else {
                out.push_str(&self.text_content);
                out.push('\n');
            }
        }

        if !self.elements.is_empty() {
            out.push_str("\nInteractive elements:\n");
            for el in &self.elements {
                let vis = if el.is_visible { "visible" } else { "hidden" };
                let ena = if el.is_enabled { "enabled" } else { "disabled" };
                let type_info = el
                    .element_type
                    .as_deref()
                    .map(|t| format!("[{}]", t))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "  [{}] {}{} \"{}\" ({}, {})\n",
                    el.id, el.tag, type_info, el.text, vis, ena
                ));
            }
        }

        if !self.forms.is_empty() {
            out.push_str("\nForms:\n");
            for (i, form) in self.forms.iter().enumerate() {
                let action = form
                    .action
                    .as_deref()
                    .unwrap_or("(no action)");
                out.push_str(&format!("  Form #{} (action: {})\n", i + 1, action));
                for field in &form.fields {
                    let field_type = field
                        .field_type
                        .as_deref()
                        .unwrap_or("text");
                    let required = if field.required { ", required" } else { "" };
                    out.push_str(&format!(
                        "    - [{}] {} ({}{})\n",
                        field.element_id, field.name, field_type, required
                    ));
                }
            }
        }

        if !self.links.is_empty() {
            out.push_str("\nLinks:\n");
            for link in self.links.iter().take(20) {
                out.push_str(&format!("  - \"{}\" → {}\n", link.text, link.href));
            }
            if self.links.len() > 20 {
                out.push_str(&format!(
                    "  ... and {} more links\n",
                    self.links.len() - 20
                ));
            }
        }

        out
    }
}

/// An interactive element on the page (button, input, link, etc.).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InteractiveElement {
    /// Stable session-scoped ID for referencing this element (e.g. "e1", "e2").
    pub id: String,
    /// HTML tag name (button, input, select, a, textarea, etc.).
    pub tag: String,
    /// ARIA role, if present.
    pub role: Option<String>,
    /// Visible text or label for this element.
    pub text: String,
    /// Input type attribute (text, password, checkbox, etc.), if applicable.
    pub element_type: Option<String>,
    /// CSS selector that uniquely targets this element.
    pub selector: String,
    /// Whether the element is currently visible in the viewport.
    pub is_visible: bool,
    /// Whether the element is enabled (not disabled).
    pub is_enabled: bool,
}

/// Information about a form on the page.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FormInfo {
    /// Form action URL.
    pub action: Option<String>,
    /// HTTP method (GET, POST, etc.).
    pub method: Option<String>,
    /// Fields belonging to this form.
    pub fields: Vec<FormField>,
}

/// A single field within a form.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FormField {
    /// Reference to the InteractiveElement ID for this field.
    pub element_id: String,
    /// Field name attribute.
    pub name: String,
    /// Field type (text, password, email, checkbox, etc.).
    pub field_type: Option<String>,
    /// Whether the field is required.
    pub required: bool,
}

/// A link on the page.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LinkInfo {
    /// Visible link text.
    pub text: String,
    /// Link target URL.
    pub href: String,
}

/// Current page loading state.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum PageState {
    /// Page is still loading.
    Loading,
    /// DOM is interactive but not all resources are loaded.
    Interactive,
    /// Page is fully loaded.
    Complete,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_page_snapshot_to_llm_text_basic() {
        let snapshot = PageSnapshot {
            url: "https://example.com".to_string(),
            title: "Example".to_string(),
            text_content: "Hello, world!".to_string(),
            elements: vec![InteractiveElement {
                id: "e1".to_string(),
                tag: "button".to_string(),
                role: None,
                text: "Click me".to_string(),
                element_type: None,
                selector: "#btn".to_string(),
                is_visible: true,
                is_enabled: true,
            }],
            forms: vec![],
            links: vec![LinkInfo {
                text: "About".to_string(),
                href: "/about".to_string(),
            }],
            state: PageState::Complete,
        };

        let text = snapshot.to_llm_text();
        assert!(text.contains("Page: Example (https://example.com)"));
        assert!(text.contains("Hello, world!"));
        assert!(text.contains("[e1] button \"Click me\" (visible, enabled)"));
        assert!(text.contains("\"About\" → /about"));
    }

    #[test]
    fn test_page_snapshot_to_llm_text_truncates_long_content() {
        let snapshot = PageSnapshot {
            url: "https://example.com".to_string(),
            title: "Long Page".to_string(),
            text_content: "x".repeat(5000),
            elements: vec![],
            forms: vec![],
            links: vec![],
            state: PageState::Complete,
        };

        let text = snapshot.to_llm_text();
        assert!(text.contains("... (truncated)"));
        assert!(text.len() < 5500);
    }

    #[test]
    fn test_page_snapshot_with_forms() {
        let snapshot = PageSnapshot {
            url: "https://example.com/login".to_string(),
            title: "Login".to_string(),
            text_content: String::new(),
            elements: vec![],
            forms: vec![FormInfo {
                action: Some("/session".to_string()),
                method: Some("POST".to_string()),
                fields: vec![
                    FormField {
                        element_id: "e1".to_string(),
                        name: "username".to_string(),
                        field_type: Some("text".to_string()),
                        required: true,
                    },
                    FormField {
                        element_id: "e2".to_string(),
                        name: "password".to_string(),
                        field_type: Some("password".to_string()),
                        required: true,
                    },
                ],
            }],
            links: vec![],
            state: PageState::Complete,
        };

        let text = snapshot.to_llm_text();
        assert!(text.contains("Form #1 (action: /session)"));
        assert!(text.contains("[e1] username (text, required)"));
        assert!(text.contains("[e2] password (password, required)"));
    }
}
