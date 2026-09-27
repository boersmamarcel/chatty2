//! The spec-run golden (HS-0, AGE-690): one small [`AgentSpec`] run against a
//! scripted fake model, with the answer and trace it produced recorded as a
//! golden.
//!
//! `fabric-hosted`'s invariant 1 needs a reference that a spec run hosted
//! (hive's HS-1, `spec_runs_same_hosted`) can compare itself against, without
//! reaching into `chatty-tui::participant::swarm_kit`, which is test-only
//! there and outside hive's dependency graph. This module is that reference,
//! published from chatty-core's own `test-support` feature so both sides can
//! depend on it.
//!
//! [`golden_spec`] and [`golden_script`] are the fixed inputs: a one-tool
//! agent and the fake model's scripted replies. [`run`] drives them through a
//! real [`AgentSession`] turn and returns the raw [`GoldenRun`].
//! `spec_golden_replays_locally` (below) masks it and compares it to the
//! stored `services/goldens/spec_run.json`; `UPDATE_GOLDENS=1` rewrites that
//! file.
//!
//! # Masked host fields
//!
//! A raw [`GoldenRun`] carries two things that vary with *where* it ran
//! rather than with the spec or the script, so they cannot be part of a
//! golden two different machines (or two different temp dirs) can agree on:
//!
//! - **Every tool call's `duration`** in the trace — wall-clock, by
//!   definition never reproducible.
//! - **Every tool call's `id`** in the trace — a run-generated identifier
//!   assigned when the trace item is created (independent of the fake
//!   model's own `call_<n>` ids), not stable across runs.
//! - **The workspace's absolute path**, wherever it appears verbatim inside a
//!   string (the answer, or a field of the trace). `list_directory`'s own
//!   args and output are workspace-relative and so carry no path today, but
//!   the mask is a blanket string replace, not a per-tool allowlist, so a
//!   future tool that does leak the absolute path is still masked.
//!
//! [`GoldenRun::masked`] does all three. **hive's HS-1 replay must mask the
//! same fields** before comparing its own trace to this golden: zero every
//! tool call's `duration` and `id`, and replace its own workspace path with
//! the literal `<workspace>` wherever it occurs.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde::{Deserialize, Serialize};

use crate::agent_spec::AgentSpec;
use crate::factories::agent_factory::{AgentBuildContext, AgentServices};
use crate::services::StreamSurface;
use crate::session::{AgentSession, AgentSessionConfig, SessionEvent, TurnInput};
use crate::settings::models::execution_settings::ExecutionSettingsModel;
use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::{ProviderConfig, ProviderType};
use crate::testing::fake_model::{FakeDaemon, Reply, Script};

/// The golden's model identifier: the fake daemon's routing key, and
/// `[agent] model` in [`GOLDEN_SPEC_TOML`].
pub const GOLDEN_MODEL: &str = "golden-spec-model";

/// The one user turn the golden runs.
pub const GOLDEN_PROMPT: &str = "What is in the workspace?";

/// The golden `AgentSpec`, as hive's HS-1 must build its own session from:
/// one tool call, then an answer.
const GOLDEN_SPEC_TOML: &str = r#"
[agent]
name = "spec-golden"
description = "HS-0's reference agent (AGE-690): lists the workspace and reports back."
model = "golden-spec-model"
preamble = "Call list_directory on \".\", then answer in one short sentence."

[tools]
profile = "coordinator"
"#;

/// Parse [`GOLDEN_SPEC_TOML`]. Panics on a malformed spec: this is a fixed
/// fixture, not user input.
pub fn golden_spec() -> AgentSpec {
    AgentSpec::from_toml(GOLDEN_SPEC_TOML).expect("the golden spec is well-formed")
}

/// The fake model's script for [`GOLDEN_MODEL`]: one `list_directory` call,
/// then the answer that ends the turn.
pub fn golden_script() -> Script {
    Script::new().route(
        GOLDEN_MODEL,
        [
            Reply::tool_call("list_directory", serde_json::json!({ "path": "." })),
            Reply::text("The workspace holds one file, README.md."),
        ],
    )
}

/// One run of the golden: the turn's answer and its trace, still carrying
/// the host-specific fields [`masked`](Self::masked) strips.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GoldenRun {
    pub answer: String,
    /// `None` only if the turn's trace had no items, which never happens for
    /// this golden's script: it always runs one tool call.
    pub trace: Option<serde_json::Value>,
}

impl GoldenRun {
    /// Mask the fields documented in the module doc: every tool call's
    /// `duration` and `id`, and `workspace`'s absolute path wherever it
    /// appears. Also canonicalizes the trace's object keys into sorted
    /// order, so the golden's byte form does not depend on whether this
    /// build's `serde_json` happens to preserve insertion order (it does
    /// under some feature combinations and not others — a plain struct
    /// field's declared order either way, never a property of the data).
    pub fn masked(mut self, workspace: &Path) -> Self {
        let workspace = workspace.to_string_lossy().into_owned();
        if let Some(trace) = &mut self.trace {
            mask_value(trace, &workspace);
            *trace = canonical(std::mem::take(trace));
        }
        self.answer = self.answer.replace(&workspace, "<workspace>");
        self
    }
}

/// Recursively null a tool call's `duration` and `id` (an object is a tool
/// call's fields when it carries `tool_name`), and replace `workspace`
/// wherever it appears inside a string.
fn mask_value(value: &mut serde_json::Value, workspace: &str) {
    match value {
        serde_json::Value::Object(map) => {
            if map.contains_key("tool_name") {
                map.insert("duration".to_string(), serde_json::Value::Null);
                map.insert(
                    "id".to_string(),
                    serde_json::Value::String("<call-id>".to_string()),
                );
            }
            for v in map.values_mut() {
                mask_value(v, workspace);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                mask_value(item, workspace);
            }
        }
        serde_json::Value::String(text) if text.contains(workspace) => {
            *text = text.replace(workspace, "<workspace>");
        }
        _ => {}
    }
}

/// Rebuild `value` with every object's keys inserted in sorted order, so its
/// serialized form is the same whether or not `serde_json`'s map happens to
/// preserve insertion order in this build.
fn canonical(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let sorted: std::collections::BTreeMap<String, serde_json::Value> =
                map.into_iter().map(|(k, v)| (k, canonical(v))).collect();
            let mut out = serde_json::Map::with_capacity(sorted.len());
            for (k, v) in sorted {
                out.insert(k, v);
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(canonical).collect())
        }
        other => other,
    }
}

/// Run the golden in `workspace`: build [`golden_spec`] into a real
/// [`AgentSession`] against a fake model scripted with [`golden_script`],
/// send [`GOLDEN_PROMPT`] as the one turn, and return the raw (unmasked)
/// result.
///
/// `workspace` must be an existing, empty directory; this writes its one
/// fixture file (`README.md`).
pub async fn run(workspace: &Path) -> GoldenRun {
    let _ = crate::init_repositories();
    let daemon = FakeDaemon::scripted(golden_script());
    std::fs::write(workspace.join("README.md"), "# Chatty\n").expect("the golden's one file");

    let settings = ExecutionSettingsModel {
        workspace_dir: Some(workspace.to_string_lossy().into_owned()),
        // The fake daemon is not the internet; keeping fetch off keeps the
        // tool block, and so the golden, smaller.
        fetch_enabled: false,
        ..ExecutionSettingsModel::default()
    };
    let spec = golden_spec();
    let built = AgentBuildContext::from_spec(
        &spec,
        AgentServices {
            exec_settings: Some(settings.clone()),
            ..AgentServices::default()
        },
    )
    .expect("the golden spec builds");

    let model_config = ModelConfig::new(
        GOLDEN_MODEL.to_string(),
        GOLDEN_MODEL.to_string(),
        ProviderType::Ollama,
        GOLDEN_MODEL.to_string(),
    );
    let provider_config = ProviderConfig::new("Golden".to_string(), ProviderType::Ollama)
        .with_base_url(daemon.base_url());

    let mut session = AgentSession::new(AgentSessionConfig {
        execution_settings: settings,
        surface: StreamSurface::InteractiveTui,
        loop_guard: false,
    });
    session
        .create_conversation(
            "spec-golden".to_string(),
            "Spec golden".to_string(),
            &model_config,
            &provider_config,
            built.context,
        )
        .await
        .expect("the golden conversation builds");

    let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
    let sink = events.clone();
    let turn = session
        .begin_turn(TurnInput::text(GOLDEN_PROMPT), move |event| {
            sink.borrow_mut().push(event)
        })
        .expect("the golden turn starts");
    turn.await;

    let events = Rc::try_unwrap(events).unwrap().into_inner();
    for event in &events {
        session.apply(event);
    }
    if let Some(SessionEvent::Error(error)) = events
        .iter()
        .find(|event| matches!(event, SessionEvent::Error(_)))
    {
        panic!("the golden turn failed: {error:?}");
    }

    let trace = session.trace_json();
    let answer = session
        .conversation()
        .and_then(|conversation| conversation.streaming_message())
        .cloned()
        .unwrap_or_default();
    session.finish_turn(None, vec![]);

    GoldenRun { answer, trace }
}

fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/services/goldens/spec_run.json")
}

/// Compare an already-[`masked`](GoldenRun::masked) run against the stored
/// golden. `UPDATE_GOLDENS=1` rewrites it, as for the other goldens in this
/// crate.
pub fn assert_golden(actual: &GoldenRun) {
    let path = golden_path();
    let recorded = serde_json::to_string_pretty(actual).expect("a GoldenRun serializes") + "\n";

    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::create_dir_all(path.parent().expect("the goldens dir")).unwrap();
        std::fs::write(&path, &recorded).unwrap();
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{} is missing ({e}); run with UPDATE_GOLDENS=1",
            path.display()
        )
    });
    assert_eq!(
        recorded,
        expected,
        "{} changed; if that is intended, rerun with UPDATE_GOLDENS=1",
        path.display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The golden replays locally: a fresh run, masked, matches the stored
    /// trace and answer (invariant 1, `spec_golden_replays_locally`).
    #[tokio::test]
    async fn spec_golden_replays_locally() {
        let workspace = tempfile::tempdir().expect("a temp workspace");
        let actual = run(workspace.path()).await.masked(workspace.path());

        assert!(
            actual.trace.is_some(),
            "the golden's script always runs one tool call"
        );
        assert_golden(&actual);
    }

    /// Two runs in different temp dirs give byte-identical masked traces:
    /// the mask really does remove every host-specific field
    /// (`spec_golden_masks_host_fields`).
    #[tokio::test]
    async fn spec_golden_masks_host_fields() {
        let workspace_a = tempfile::tempdir().expect("a temp workspace");
        let workspace_b = tempfile::tempdir().expect("a different temp workspace");
        assert_ne!(workspace_a.path(), workspace_b.path());

        let a = run(workspace_a.path()).await.masked(workspace_a.path());
        let b = run(workspace_b.path()).await.masked(workspace_b.path());

        let a_json = serde_json::to_string_pretty(&a).unwrap();
        let b_json = serde_json::to_string_pretty(&b).unwrap();
        assert_eq!(
            a_json, b_json,
            "masked traces recorded in different temp dirs must be byte-identical"
        );
    }
}
