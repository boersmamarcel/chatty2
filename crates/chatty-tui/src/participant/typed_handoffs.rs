//! TD-2 (AGE-693): typed handoffs on the swarm kit.
//!
//! A team file names a JSON Schema per role (`handoffs`); the worker's
//! final answer for that role must carry one fenced `json` block matching
//! it. These tests run a real root broker and real `chatty-tui` workers
//! against the kit's scripted fake model, so what they check is what a
//! leader's `invoke_agent` sees and what crosses the worker's socket.

use serde_json::{Value, json};

use super::swarm_kit::{AgentDef, Endpoint, SwarmKit, normalize};
use chatty_core::services::handoff::{HANDOFF_FOLLOW_UP_PREFIX, HandoffContract, HandoffLedger};
use chatty_core::testing::fake_model::{Reply, Script};
use chatty_core::tools::invoke_agent_tool::{InvokeAgentArgs, InvokeAgentError};
use rig_agent::tool::{Tool, ToolContext};

const CODER: &str = "kit-coder";
const CODER_MODEL: &str = "kit/coder";

/// `value` with every object's keys in sorted order, so a line reads the
/// same whether or not serde_json's `preserve_order` is on in this build
/// (GPUI turns it on under `--all-features`).
fn canonical(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: std::collections::BTreeMap<String, Value> =
                map.into_iter().map(|(k, v)| (k, canonical(v))).collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
        other => other,
    }
}

/// Replace the fields that are a clock reading, a generated task id or a
/// build's version rather than behaviour.
fn scrub(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                match key.as_str() {
                    "at" | "duration_ms" | "durationMs" => *v = json!("<CLOCK>"),
                    "taskId" => *v = json!("<TASK>"),
                    "version" if v.is_string() => *v = json!("<VERSION>"),
                    _ => scrub(v),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(scrub),
        _ => {}
    }
}

/// One tapped line with its direction kept, its JSON canonical and scrubbed,
/// and the kit's paths, ports and ids normalised.
fn wire_line(kit: &SwarmKit, line: &str) -> String {
    let (direction, frame) = line.split_at(3);
    let mut value: Value = serde_json::from_str(frame).expect("a frame is JSON");
    scrub(&mut value);
    let frame = serde_json::to_string(&canonical(value)).expect("JSON serializes");
    normalize(kit, &format!("{direction}{frame}"))
}

/// Invariant 4: a team without `handoffs` sends no handoff field. The
/// golden pins the whole wire (canonical key order, clocks and the version
/// scrubbed); it was recorded on `main` before TD-2 and re-recorded only by
/// ADR-0021's envelope changes (EN-1, and step 3), each change explained by
/// a row of that ADR's frame-to-method map.
#[tokio::test]
async fn handoff_absent_is_unchanged() {
    let kit = SwarmKit::start(
        vec![AgentDef::new(CODER, CODER_MODEL, Endpoint::Sse)],
        Script::new().route(
            CODER_MODEL,
            [
                Reply::tool_call("read_file", json!({ "path": "README.md" })),
                Reply::text("Done.\n\n```json\n{\"files_changed\": [\"README.md\"]}\n```"),
            ],
        ),
        Script::new(),
    )
    .await;
    let mut tap = kit.participants().tap_wire();

    let run = kit.run_leader("change the readme").await;
    let out = run.output.as_ref().expect("the delegation succeeded");
    assert!(out.success);

    let mut lines = Vec::new();
    while let Ok(line) = tap.try_recv() {
        lines.push(wire_line(&kit, &line));
    }
    for line in &lines {
        assert!(!line.contains("\"handoff\""), "a handoff field: {line}");
    }
    let recorded = format!("{}\n", lines.join("\n"));
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/participant/goldens/handoff_absent_wire.txt");
    let Ok(expected) = std::fs::read_to_string(&path) else {
        if std::env::var("RECORD_HANDOFF_ABSENT_GOLDEN").is_ok() {
            std::fs::write(&path, &recorded).expect("golden written");
            return;
        }
        panic!("missing golden {}", path.display());
    };
    assert_eq!(
        expected,
        recorded,
        "a team without handoffs changed the wire ({})",
        path.display()
    );
}

/// The coder's handoff schema in these tests: the files it changed.
fn change_schema() -> Value {
    json!({
        "type": "object",
        "required": ["files_changed"],
        "properties": {
            "files_changed": { "type": "array", "items": { "type": "string" } }
        }
    })
}

fn contract() -> HandoffContract {
    HandoffContract {
        role: CODER.to_string(),
        schema: chatty_fabric::wire::Opaque::from_value(&change_schema()).unwrap(),
    }
}

const VALID: &str = "Changed it.\n\n```json\n{\"files_changed\": [\"README.md\"]}\n```";

/// A kit whose one role, the coder, has a handoff schema and answers with
/// `replies` in order.
async fn coder_kit(replies: Vec<Reply>) -> SwarmKit {
    SwarmKit::start(
        vec![AgentDef::new(CODER, CODER_MODEL, Endpoint::Sse).with_handoff(change_schema())],
        Script::new().route(CODER_MODEL, replies),
        Script::new(),
    )
    .await
}

fn body(request: &chatty_core::testing::fake_model::RecordedRequest) -> String {
    String::from_utf8_lossy(&request.body).into_owned()
}

/// Invariant 2: a valid handoff passes through untouched. The worker is
/// told its schema with its task, answers once, and the leader's
/// `invoke_agent` result carries the parsed JSON as `handoff`.
#[tokio::test]
async fn handoff_valid_passes() {
    let kit = coder_kit(vec![Reply::text(VALID)]).await;
    let ledger = HandoffLedger::new([&contract()]);
    let run = kit
        .run_leader_with(
            kit.leader_tool().with_handoff_ledger(ledger.clone()),
            CODER,
            "change the readme",
        )
        .await;

    let out = run.output.as_ref().expect("the delegation succeeded");
    assert!(out.success);
    assert_eq!(
        out.handoff,
        Some(json!({ "files_changed": ["README.md"] })),
        "the handoff rides the result"
    );
    let serialized = serde_json::to_value(out).unwrap();
    assert_eq!(serialized["handoff"]["files_changed"][0], "README.md");

    let requests = kit.sse.requests_for(CODER_MODEL);
    assert_eq!(requests.len(), 1, "a valid first answer is not sent back");
    assert!(
        body(&requests[0]).contains("handing off as the `kit-coder` role"),
        "the worker is told its schema with its task"
    );
    assert!(ledger.invalid_by_role().is_empty());
}

/// Invariant 3, first half: an answer without a valid handoff is sent back
/// exactly once, the extra request naming the schema errors; the second,
/// valid answer passes and the invalid one is counted for the role.
#[tokio::test]
async fn handoff_invalid_reprompts_once() {
    let kit = coder_kit(vec![Reply::text("Changed it."), Reply::text(VALID)]).await;
    let ledger = HandoffLedger::new([&contract()]);
    let run = kit
        .run_leader_with(
            kit.leader_tool().with_handoff_ledger(ledger.clone()),
            CODER,
            "change the readme",
        )
        .await;

    let out = run.output.as_ref().expect("the second answer is valid");
    assert_eq!(out.handoff, Some(json!({ "files_changed": ["README.md"] })));

    let requests = kit.sse.requests_for(CODER_MODEL);
    assert_eq!(
        requests.len(),
        2,
        "exactly one extra request for the worker"
    );
    let retry = body(&requests[1]);
    assert!(
        retry.contains(HANDOFF_FOLLOW_UP_PREFIX),
        "the extra request is the handoff re-prompt"
    );
    assert!(
        retry.contains("no fenced ```json block"),
        "the re-prompt names the schema error"
    );
    assert!(
        !body(&requests[0]).contains(HANDOFF_FOLLOW_UP_PREFIX),
        "the first request is the task"
    );
    assert_eq!(ledger.invalid_by_role()[CODER], 1);
}

/// Invariant 3, second half: a handoff that fails its schema again after
/// the one re-prompt fails the task with `HandoffInvalid`, which the model
/// sees as `Error: invoke_agent: handoff_invalid: <role>: …`. There is no
/// third request.
#[tokio::test]
async fn handoff_invalid_twice_fails_typed() {
    let kit = coder_kit(vec![
        Reply::text("Changed it."),
        Reply::text("Changed it.\n\n```json\n{\"files_changed\": \"README.md\"}\n```"),
    ])
    .await;
    let ledger = HandoffLedger::new([&contract()]);
    let tool = kit.leader_tool().with_handoff_ledger(ledger.clone());

    let error = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        tool.call(
            &mut ToolContext::new(),
            InvokeAgentArgs {
                agent: CODER.to_string(),
                prompt: "change the readme".to_string(),
                include_trace: false,
            },
        ),
    )
    .await
    .expect("the delegation ends before the deadline")
    .expect_err("a second invalid handoff fails the task");

    let InvokeAgentError::HandoffInvalid { role, errors, .. } = &error else {
        panic!("expected HandoffInvalid, got {error:?}");
    };
    assert_eq!(role, CODER);
    assert!(
        errors.iter().any(|e| e.starts_with("/files_changed:")),
        "{errors:?}"
    );
    let seen = tool.map_error(error).to_string();
    assert!(
        seen.starts_with("Error: invoke_agent: handoff_invalid: kit-coder: /files_changed:"),
        "{seen}"
    );
    assert_eq!(
        kit.sse.requests_for(CODER_MODEL).len(),
        2,
        "one re-prompt, then the task fails"
    );
    assert_eq!(ledger.invalid_by_role()[CODER], 2);
}
