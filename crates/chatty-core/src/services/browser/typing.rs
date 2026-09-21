//! Agent-driven text entry (AGE-492): one element ref and a string in, the
//! field's contents replaced.
//!
//! Built on [`super::click`]: the field is located and verified exactly the
//! way a click target is (same element as the snapshot, on screen, nothing
//! covering it) and focused by the same pointer events. Two rules on top:
//!
//! - **Credential fields are refused before anything is sent.** A password
//!   input or a payment-card field is the user's to fill under take-control
//!   (AGE-158); the agent never gets to try.
//! - **Replace, never append.** Select-all via the key event's editing
//!   command — platform-independent, no Ctrl/Cmd guessing — then
//!   `Input.insertText`. What the agent asked for is what the field holds,
//!   whatever was there before.
//!
//! The field's accessible value is read back afterwards so the caller can
//! tell whether the text was actually accepted.

use chromiumoxide::cdp::browser_protocol::accessibility::GetPartialAxTreeParams;
use chromiumoxide::cdp::browser_protocol::dom::{BackendNodeId, DescribeNodeParams};
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, InsertTextParams,
};
use chromiumoxide::page::Page;

use super::click;
use super::error::BrowserError;
use super::snapshot::SnapshotNode;

/// Longest string `browser_type` accepts. Long enough for a document pasted
/// into a textarea; short enough that a runaway model cannot stall the
/// browser inserting megabytes.
pub const MAX_TEXT_LEN: usize = 10_000;

/// Roles whose accessible value is the field's contents, so the read-back
/// can be checked rather than merely reported.
fn value_is_checkable(role: &str) -> bool {
    matches!(role, "textbox" | "searchbox" | "combobox")
}

/// Replace the contents of `node` with `text`.
pub(super) async fn type_text(
    page: &Page,
    node: &SnapshotNode,
    text: &str,
) -> Result<String, BrowserError> {
    let r#ref = &node.r#ref;
    let (target, point) = click::locate(page, node).await?;
    ensure_not_credential_field(page, target, r#ref).await?;

    click::press(page, point).await?;
    select_all(page).await?;
    page.execute(InsertTextParams::new(text.to_string()))
        .await
        .map_err(|e| BrowserError::Protocol(format!("cannot insert text into ref {ref}: {e}")))?;

    let value = read_value(page, target).await?;
    if value_is_checkable(&node.role) && !value.contains(text) {
        return Err(BrowserError::Protocol(format!(
            "ref {ref} did not accept the text: it now reads \"{value}\"; the field may be \
             read-only, or a script may have rewritten it — take a new snapshot"
        )));
    }
    Ok(value)
}

/// Refuse a password or payment-card input (AGE-158).
async fn ensure_not_credential_field(
    page: &Page,
    target: BackendNodeId,
    r#ref: &str,
) -> Result<(), BrowserError> {
    let described = page
        .execute(
            DescribeNodeParams::builder()
                .backend_node_id(target)
                .build(),
        )
        .await
        .map_err(|e| BrowserError::Protocol(format!("cannot inspect ref {ref}: {e}")))?;
    let node = &described.result.node;
    if let Some(kind) = credential_kind(&node.local_name, node.attributes.as_deref()) {
        return Err(BrowserError::Protocol(format!(
            "ref {ref} is a {kind} field; the agent never enters credentials or payment \
             details — ask the user to take control of the browser and fill it in themselves"
        )));
    }
    Ok(())
}

/// What kind of credential field an element is, if any. `attributes` is
/// CDP's flat `[name, value, name, value, …]` list.
fn credential_kind(local_name: &str, attributes: Option<&[String]>) -> Option<&'static str> {
    let attr = |wanted: &str| {
        attributes?
            .as_chunks::<2>()
            .0
            .iter()
            .find(|[name, _]| name.eq_ignore_ascii_case(wanted))
            .map(|[_, value]| value.as_str())
    };
    if local_name.eq_ignore_ascii_case("input")
        && attr("type").is_some_and(|t| t.eq_ignore_ascii_case("password"))
    {
        return Some("password");
    }
    if attr("autocomplete").is_some_and(|a| a.to_ascii_lowercase().starts_with("cc-")) {
        return Some("payment card");
    }
    None
}

/// Select everything in the focused field. The `commands` list on a key
/// event runs Blink's editing command directly, so this works the same on
/// every platform — unlike Ctrl+A, which is "go to line start" on a Mac.
async fn select_all(page: &Page) -> Result<(), BrowserError> {
    for (kind, commands) in [
        (
            DispatchKeyEventType::RawKeyDown,
            vec!["selectAll".to_string()],
        ),
        (DispatchKeyEventType::KeyUp, Vec::new()),
    ] {
        let params = DispatchKeyEventParams::builder()
            .r#type(kind)
            .key("a")
            .code("KeyA")
            .windows_virtual_key_code(65)
            .native_virtual_key_code(65)
            .commands(commands)
            .build()
            .map_err(|e| BrowserError::Protocol(format!("invalid key event: {e}")))?;
        page.execute(params)
            .await
            .map_err(|e| BrowserError::Protocol(format!("select-all failed: {e}")))?;
    }
    Ok(())
}

/// The field's accessible value right now.
async fn read_value(page: &Page, target: BackendNodeId) -> Result<String, BrowserError> {
    let live = page
        .execute(
            GetPartialAxTreeParams::builder()
                .backend_node_id(target)
                .fetch_relatives(false)
                .build(),
        )
        .await
        .map_err(|e| BrowserError::Protocol(format!("cannot read the field back: {e}")))?;
    Ok(live
        .result
        .nodes
        .iter()
        .find(|n| n.backend_dom_node_id == Some(target))
        .map(|n| click::ax_value(n.value.as_ref()))
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs(pairs: &[(&str, &str)]) -> Vec<String> {
        pairs
            .iter()
            .flat_map(|(k, v)| [k.to_string(), v.to_string()])
            .collect()
    }

    #[test]
    fn password_inputs_are_credential_fields() {
        let a = attrs(&[("name", "pw"), ("type", "password")]);
        assert_eq!(credential_kind("input", Some(&a)), Some("password"));
        let a = attrs(&[("TYPE", "Password")]);
        assert_eq!(credential_kind("INPUT", Some(&a)), Some("password"));
        // Only inputs have a password type; a div with type=password is just a div.
        assert_eq!(credential_kind("div", Some(&a)), None);
    }

    #[test]
    fn payment_card_autocomplete_is_a_credential_field_on_any_element() {
        let a = attrs(&[("autocomplete", "cc-number")]);
        assert_eq!(credential_kind("input", Some(&a)), Some("payment card"));
        let a = attrs(&[("autocomplete", "CC-CSC")]);
        assert_eq!(credential_kind("div", Some(&a)), Some("payment card"));
    }

    #[test]
    fn ordinary_fields_are_not_credential_fields() {
        let a = attrs(&[("type", "email"), ("autocomplete", "email")]);
        assert_eq!(credential_kind("input", Some(&a)), None);
        assert_eq!(credential_kind("textarea", None), None);
        assert_eq!(credential_kind("input", Some(&[])), None);
    }

    #[test]
    fn only_text_roles_have_a_checkable_value() {
        assert!(value_is_checkable("textbox"));
        assert!(value_is_checkable("searchbox"));
        assert!(value_is_checkable("combobox"));
        assert!(!value_is_checkable("generic"));
        assert!(!value_is_checkable("button"));
    }
}
