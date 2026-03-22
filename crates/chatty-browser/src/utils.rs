//! Shared utilities used across the chatty-browser crate.

use crate::session::escape_js_string;

/// Truncate a string to approximately `max_len` characters at a word boundary,
/// respecting UTF-8 boundaries. Appends '…' if truncated.
pub fn truncate_text(text: &str, max_len: usize) -> String {
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

/// Build a JS snippet that fills a form field using `nativeInputValueSetter`
/// for React/Vue/Angular compatibility, then dispatches `input`, `change`,
/// and `blur` events.
///
/// Returns a self-invoking JS function that returns JSON:
/// `{ "success": true }` or `{ "error": "..." }`.
pub fn fill_field_js(selector: &str, value: &str, field_label: &str) -> String {
    let sel = escape_js_string(selector);
    let val = escape_js_string(value);
    let label = escape_js_string(field_label);
    format!(
        r#"(() => {{
            const el = document.querySelector("{sel}");
            if (!el) return JSON.stringify({{ error: "{label} field not found: {sel}" }});
            const setter = Object.getOwnPropertyDescriptor(
                window.HTMLInputElement.prototype, 'value'
            )?.set || Object.getOwnPropertyDescriptor(
                window.HTMLTextAreaElement.prototype, 'value'
            )?.set;
            if (setter) {{
                setter.call(el, "{val}");
            }} else {{
                el.value = "{val}";
            }}
            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
            el.dispatchEvent(new Event('blur', {{ bubbles: true }}));
            return JSON.stringify({{ success: true }});
        }})()"#
    )
}
