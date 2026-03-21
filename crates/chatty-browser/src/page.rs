use serde::{Deserialize, Serialize};

// ── PageSnapshot ─────────────────────────────────────────────────────────────

/// Structured view of a web page, designed for LLM consumption.
///
/// The LLM never sees raw HTML. Instead, `PageSnapshot` provides a compact,
/// structured representation with truncated text, interactive elements with
/// stable IDs (`e1`, `e2`, …), and auto-detected login hints.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PageSnapshot {
    pub url: String,
    pub title: String,
    /// Main text content, truncated to ~3000 chars.
    pub text_content: String,
    /// Interactive elements (buttons, inputs, links) with stable IDs.
    /// Capped at 50 elements.
    pub elements: Vec<InteractiveElement>,
    /// Detected forms on the page.
    pub forms: Vec<FormInfo>,
    /// Links found on the page, capped at 50.
    pub links: Vec<LinkInfo>,
    /// Auto-detected login hint when a password field is present
    /// and a matching `LoginProfile` exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login_hint: Option<LoginHint>,
    /// OpenGraph image URL, if found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub og_image_url: Option<String>,
    /// Page meta description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl std::fmt::Display for PageSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "# {}", self.title)?;
        writeln!(f, "URL: {}", self.url)?;
        writeln!(f)?;

        if !self.text_content.is_empty() {
            writeln!(f, "## Content")?;
            writeln!(f, "{}", self.text_content)?;
            writeln!(f)?;
        }

        if !self.elements.is_empty() {
            writeln!(f, "## Interactive Elements")?;
            for el in &self.elements {
                writeln!(f, "- [{}] {} ({})", el.id, el.label, el.element_type)?;
            }
            writeln!(f)?;
        }

        if !self.forms.is_empty() {
            writeln!(f, "## Forms")?;
            for form in &self.forms {
                writeln!(f, "- {} ({} fields)", form.name_or_id(), form.fields.len())?;
            }
            writeln!(f)?;
        }

        if !self.links.is_empty() {
            writeln!(f, "## Links")?;
            for link in &self.links {
                writeln!(f, "- [{}]({})", link.text, link.href)?;
            }
        }

        if let Some(hint) = &self.login_hint {
            writeln!(f)?;
            writeln!(
                f,
                "⚠ Login detected: use `browser_auth {{ credential_name: \"{}\" }}`",
                hint.credential_name
            )?;
        }

        Ok(())
    }
}

// ── Interactive elements ────────────────────────────────────────────────────

/// An interactive element on the page.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InteractiveElement {
    /// Stable ID for the element (e.g., "e1", "e2").
    pub id: String,
    /// The type of element (button, input, link, select, textarea).
    pub element_type: String,
    /// Human-readable label or visible text.
    pub label: String,
    /// For input fields: the current value (passwords always redacted as "●●●●").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// For inputs: the `name` attribute if present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// For inputs: the `type` attribute (text, password, email, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,
    /// For links: the `href` attribute.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    /// Whether the element is visible (within viewport).
    #[serde(default = "default_true")]
    pub visible: bool,
    /// Whether the element is disabled.
    #[serde(default)]
    pub disabled: bool,
}

fn default_true() -> bool {
    true
}

// ── Forms ───────────────────────────────────────────────────────────────────

/// Description of a form on the page.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FormInfo {
    /// Element ID of the form (e.g., "e5"), if it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The `name` attribute of the form.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The `action` URL of the form.
    pub action: String,
    /// HTTP method (GET or POST).
    pub method: String,
    /// Fields contained in this form.
    pub fields: Vec<FormField>,
}

impl FormInfo {
    /// Return a display name for the form.
    pub fn name_or_id(&self) -> &str {
        self.name
            .as_deref()
            .or(self.id.as_deref())
            .unwrap_or("(unnamed)")
    }
}

/// A field within a form.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FormField {
    /// The field's `name` attribute.
    pub name: String,
    /// The field type (text, password, email, hidden, select, textarea, etc.).
    pub field_type: String,
    /// Current value (passwords always redacted).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Whether the field is required.
    #[serde(default)]
    pub required: bool,
    /// Placeholder text, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
}

// ── Links ───────────────────────────────────────────────────────────────────

/// A link on the page.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LinkInfo {
    /// The visible text of the link.
    pub text: String,
    /// The `href` attribute.
    pub href: String,
}

// ── Login hint ──────────────────────────────────────────────────────────────

/// Injected into `PageSnapshot` when a password field is detected and a
/// matching `LoginProfile` exists.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoginHint {
    /// The name of the stored credential (e.g., "komoot").
    pub credential_name: String,
    /// A human-readable message for the LLM.
    pub message: String,
}
