//! `SessionEvent`'s canonical JSON Schema (AGE-2103, HX-1b).
//!
//! `chatty-server` puts `SessionEvent` on the wire as SSE frames, so the
//! shape a hosted client must decode is this one. hive's `GET /api/version`
//! hashes a schema export of those frames; this is that export for the
//! session half, generated from the Rust types with `schemars` (never
//! hand-written), in the same canonical form as
//! `chatty_fabric::wire::schema`.
//!
//! `crates/chatty-core/schema/session-event.json` is the checked-in export;
//! `tests::schema_export_is_up_to_date` fails on drift and `UPDATE_GOLDENS=1`
//! regenerates it. The derives change no serialized shape.
//!
//! Two payloads are opaque (`serde_json::Value`): rig's `Message` (the turn
//! messages carry rig's own JSON, which has no schema) and the fabric's
//! captured-conversation messages.

use schemars::generate::SchemaSettings;
use serde_json::Value;

use super::SessionEvent;

/// Recursively sort object keys and drop rustdoc `description`s, so the bytes
/// do not depend on map order or doc wording.
fn canonicalize(value: &mut Value) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(canonicalize),
        Value::Object(map) => {
            map.remove("description");
            map.values_mut().for_each(canonicalize);
            let mut entries: Vec<_> = std::mem::take(map).into_iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.cmp(b));
            map.extend(entries);
        }
        _ => {}
    }
}

/// The schema of [`SessionEvent`], canonicalized.
pub fn export() -> Value {
    let schema = SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<SessionEvent>();
    let mut value = schema.to_value();
    canonicalize(&mut value);
    value
}

/// [`export`] as the checked-in bytes: sorted keys, two-space indent, one
/// trailing newline.
pub fn canonical_bytes() -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(&export()).expect("the export always serializes");
    bytes.push(b'\n');
    bytes
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    fn golden_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/session-event.json")
    }

    #[test]
    fn schema_export_is_up_to_date() {
        let generated = canonical_bytes();
        let path = golden_path();
        if std::env::var("UPDATE_GOLDENS").is_ok() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &generated).unwrap();
            return;
        }
        let committed = std::fs::read(&path).unwrap_or_else(|_| {
            panic!(
                "missing schema export {}; re-run with UPDATE_GOLDENS=1",
                path.display()
            )
        });
        assert!(
            committed == generated,
            "SessionEvent's schema drifted from {}; if the change is deliberate, \
             re-run with UPDATE_GOLDENS=1 and say why in the PR",
            path.display()
        );
    }

    /// Every variant is in the export, so a new one cannot slip in unnoticed
    /// by a reader of the diff alone.
    #[test]
    fn export_names_every_variant() {
        let text = String::from_utf8(canonical_bytes()).unwrap();
        for variant in [
            "TurnStarted",
            "Text",
            "ToolCallStarted",
            "ToolCallInput",
            "ToolCallResult",
            "ToolCallError",
            "ApprovalRequested",
            "ApprovalResolved",
            "ClarificationRequested",
            "ApiCallUsage",
            "TokenUsage",
            "PluginUsage",
            "TurnMessages",
            "Delegation",
            "SwarmEvent",
            "Error",
            "Cancelled",
            "TurnEnded",
            "FollowUp",
        ] {
            assert!(text.contains(&format!("\"{variant}\"")), "{variant}");
        }
    }
}
