//! Agent-driven dropdowns (AGE-2009): a `<select>`'s ref and an option in,
//! the option selected.
//!
//! A click cannot pick from a native dropdown in headless Chrome (the popup
//! is a separate widget the page never gets), and `browser_type` is for text
//! fields. So the option is set on the element itself, the way a user's pick
//! ends up: `selectedIndex`, then `input` and `change` events that bubble, so
//! the page's own handlers (and a form's later submit) see what a person's
//! choice would have produced.
//!
//! The element is located and verified like a click target (same element as
//! the snapshot, on screen, nothing covering it) before anything is set.

use std::collections::HashMap;

use chromiumoxide::cdp::browser_protocol::dom::{BackendNodeId, ResolveNodeParams};
use chromiumoxide::cdp::js_protocol::runtime::{
    CallArgument, CallFunctionOnParams, RemoteObjectId,
};
use chromiumoxide::page::Page;
use serde::Deserialize;
use tracing::debug;

use super::click;
use super::error::BrowserError;
use super::snapshot::SnapshotNode;

/// Most option labels an error or a snapshot line lists.
pub const MAX_LISTED_OPTIONS: usize = 30;

/// Sets the option on `this` (a `<select>`). Matches the visible label first,
/// then the value; whitespace-collapsed, then case-insensitive. Returns a
/// JSON-able object, never throws.
const SELECT_JS: &str = r#"function (wanted) {
  const el = this;
  if (!el || el.tagName !== 'SELECT') {
    return { kind: 'not_select', tag: el && el.tagName ? el.tagName.toLowerCase() : '' };
  }
  const norm = (s) => String(s).replace(/\s+/g, ' ').trim();
  const opts = Array.from(el.options);
  const labels = opts.map((o) => norm(o.label));
  const want = norm(wanted);
  const lower = want.toLowerCase();
  let hit = opts.find((o) => norm(o.label) === want)
    || opts.find((o) => o.value === wanted || norm(o.value) === want)
    || opts.find((o) => norm(o.label).toLowerCase() === lower)
    || opts.find((o) => norm(o.value).toLowerCase() === lower);
  if (!hit) return { kind: 'unknown', labels };
  if (el.disabled) return { kind: 'disabled_select' };
  if (hit.disabled) return { kind: 'disabled_option', label: norm(hit.label) };
  el.focus();
  if (el.multiple) {
    opts.forEach((o) => { o.selected = (o === hit); });
  } else {
    el.selectedIndex = hit.index;
  }
  el.dispatchEvent(new Event('input', { bubbles: true, composed: true }));
  el.dispatchEvent(new Event('change', { bubbles: true }));
  const now = el.selectedIndex >= 0 ? el.options[el.selectedIndex] : null;
  return { kind: 'ok', label: now ? norm(now.label) : '' };
}"#;

/// Reads a select's current option and its labels, or says it is not one.
const DESCRIBE_JS: &str = r#"function () {
  const el = this;
  if (!el || el.tagName !== 'SELECT') return { kind: 'not_select' };
  const norm = (s) => String(s).replace(/\s+/g, ' ').trim();
  const labels = Array.from(el.options).map((o) => norm(o.label));
  const selected = Array.from(el.selectedOptions).map((o) => norm(o.label));
  return { kind: 'select', labels, selected, multiple: el.multiple };
}"#;

/// What the page-side script reported.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Outcome {
    Ok {
        label: String,
    },
    Unknown {
        labels: Vec<String>,
    },
    NotSelect {
        #[serde(default)]
        tag: String,
    },
    DisabledSelect,
    DisabledOption {
        label: String,
    },
    Select {
        labels: Vec<String>,
        selected: Vec<String>,
        #[serde(default)]
        multiple: bool,
    },
}

/// Select `option` (a visible label or a value) in the `<select>` `node`
/// and return the label now selected.
pub(super) async fn select_option(
    page: &Page,
    node: &SnapshotNode,
    option: &str,
) -> Result<String, BrowserError> {
    let r#ref = &node.r#ref;
    let (target, _) = click::locate(page, node).await?;
    match call(page, target, SELECT_JS, Some(option)).await? {
        Outcome::Ok { label } => Ok(label),
        other => Err(BrowserError::Protocol(refusal(r#ref, option, &other))),
    }
}

/// Describe every `<select>` among `nodes` for the snapshot: ref → text such
/// as `selected "Custom"; options: "Custom", "USPS"`. Elements that are not
/// selects, and any that cannot be read, are left out.
pub(super) async fn describe_selects(
    page: &Page,
    nodes: &[SnapshotNode],
) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for node in nodes {
        if !matches!(node.role.as_str(), "combobox" | "listbox") {
            continue;
        }
        let Some(id) = node.backend_node_id else {
            continue;
        };
        match call(page, BackendNodeId::new(id), DESCRIBE_JS, None).await {
            Ok(Outcome::Select {
                labels,
                selected,
                multiple,
            }) => {
                out.insert(
                    node.r#ref.clone(),
                    select_detail(&labels, &selected, multiple),
                );
            }
            Ok(_) => {}
            Err(e) => debug!(r#ref = %node.r#ref, error = %e, "browser: cannot read select"),
        }
    }
    out
}

/// Run `function` with the element as `this`.
async fn call(
    page: &Page,
    target: BackendNodeId,
    function: &str,
    argument: Option<&str>,
) -> Result<Outcome, BrowserError> {
    let resolved = page
        .execute(ResolveNodeParams::builder().backend_node_id(target).build())
        .await
        .map_err(|e| BrowserError::Protocol(format!("cannot reach the element: {e}")))?;
    let object_id: RemoteObjectId = resolved.result.object.object_id.ok_or_else(|| {
        BrowserError::Protocol("the element is not a DOM node that can be driven".to_string())
    })?;
    let mut params = CallFunctionOnParams::builder()
        .function_declaration(function)
        .object_id(object_id)
        .return_by_value(true);
    if let Some(argument) = argument {
        params = params.argument(
            CallArgument::builder()
                .value(serde_json::Value::String(argument.to_string()))
                .build(),
        );
    }
    let params = params
        .build()
        .map_err(|e| BrowserError::Protocol(format!("invalid script call: {e}")))?;
    let response = page
        .execute(params)
        .await
        .map_err(|e| BrowserError::Protocol(format!("cannot set the option: {e}")))?;
    if let Some(exception) = response.result.exception_details {
        return Err(BrowserError::Protocol(format!(
            "the page rejected the selection: {}",
            exception.text
        )));
    }
    let value = response.result.result.value.ok_or_else(|| {
        BrowserError::Protocol("the page returned nothing for the selection".to_string())
    })?;
    serde_json::from_value(value)
        .map_err(|e| BrowserError::Protocol(format!("unreadable selection result: {e}")))
}

/// `"a", "b", "c" (and 4 more)`: at most [`MAX_LISTED_OPTIONS`] labels.
fn list_labels(labels: &[String]) -> String {
    let shown = labels
        .iter()
        .take(MAX_LISTED_OPTIONS)
        .map(|l| format!("\"{l}\""))
        .collect::<Vec<_>>()
        .join(", ");
    match labels.len().saturating_sub(MAX_LISTED_OPTIONS) {
        0 => shown,
        more => format!("{shown} (and {more} more)"),
    }
}

/// The snapshot's note on a dropdown: what is selected, and what could be.
fn select_detail(labels: &[String], selected: &[String], multiple: bool) -> String {
    let kind = if multiple { "multi-select" } else { "dropdown" };
    let current = if selected.is_empty() {
        "nothing selected".to_string()
    } else {
        format!("selected {}", list_labels(selected))
    };
    format!(
        "({kind}; {current}; options: {}) — set it with browser_select",
        list_labels(labels)
    )
}

/// Why the selection did not happen, as the model reads it.
fn refusal(r#ref: &str, option: &str, outcome: &Outcome) -> String {
    match outcome {
        Outcome::Unknown { labels } => format!(
            "ref {ref} has no option \"{option}\"; the available options are: {}",
            list_labels(labels)
        ),
        Outcome::NotSelect { tag } => {
            let what = if tag.is_empty() { "element" } else { tag };
            format!(
                "ref {ref} is a {what}, not a dropdown (<select>); browser_select only sets \
                 dropdowns — use browser_type for text fields or browser_click for the rest"
            )
        }
        Outcome::DisabledSelect => format!("ref {ref} is disabled and cannot be changed"),
        Outcome::DisabledOption { label } => {
            format!("the option \"{label}\" of ref {ref} is disabled and cannot be selected")
        }
        _ => format!("ref {ref} could not be set to \"{option}\""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("Option {i}")).collect()
    }

    #[test]
    fn an_unknown_option_lists_the_labels() {
        let outcome = Outcome::Unknown {
            labels: vec!["Custom".into(), "USPS".into()],
        };
        let message = refusal("e4", "fedex", &outcome);
        assert!(message.contains("no option \"fedex\""), "{message}");
        assert!(message.contains("\"Custom\", \"USPS\""), "{message}");
    }

    #[test]
    fn at_most_thirty_labels_are_listed() {
        let listed = list_labels(&labels(45));
        assert!(listed.contains("\"Option 29\""), "{listed}");
        assert!(!listed.contains("\"Option 30\""), "{listed}");
        assert!(listed.ends_with("(and 15 more)"), "{listed}");
        assert!(!list_labels(&labels(30)).contains("more"));
    }

    #[test]
    fn a_non_select_says_what_it_is() {
        let message = refusal("e2", "x", &Outcome::NotSelect { tag: "div".into() });
        assert!(message.contains("is a div, not a dropdown"), "{message}");
    }

    #[test]
    fn the_snapshot_note_shows_the_current_option_and_the_options() {
        let note = select_detail(&["Custom".into(), "USPS".into()], &["Custom".into()], false);
        assert!(note.contains("dropdown"), "{note}");
        assert!(note.contains("selected \"Custom\""), "{note}");
        assert!(note.contains("options: \"Custom\", \"USPS\""), "{note}");
    }

    #[test]
    fn the_page_script_result_parses() {
        let ok: Outcome =
            serde_json::from_value(serde_json::json!({"kind": "ok", "label": "USPS"})).unwrap();
        assert_eq!(
            ok,
            Outcome::Ok {
                label: "USPS".into()
            }
        );
        let unknown: Outcome =
            serde_json::from_value(serde_json::json!({"kind": "unknown", "labels": ["a"]}))
                .unwrap();
        assert_eq!(
            unknown,
            Outcome::Unknown {
                labels: vec!["a".into()]
            }
        );
    }
}
