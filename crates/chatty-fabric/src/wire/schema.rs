//! The wire's canonical JSON Schema export, and its hash (ADR-0021 § 1,
//! EN-3b).
//!
//! `export()` generates the schema straight from the wire types with
//! `schemars` — it is never hand-written. [`Catalog`] is the only
//! hand-wired part: a struct with one field per request, notification or
//! result the participant protocol v3 envelope carries (ADR-0021 § 1's
//! method table), named after the wire method. Every field's *shape* comes
//! from `#[derive(JsonSchema)]` on the type itself.
//!
//! `crates/chatty-fabric/schema/fabric-v3.json` is this export's checked-in,
//! canonical form (`tests::schema_export_is_up_to_date` fails CI on drift;
//! re-run it with `UPDATE_GOLDENS=1` to regenerate). [`hash`] reads that
//! file at compile time (`include_str!`) and hashes it at first use, so a
//! build needs no `schemars` at runtime, only to regenerate the export.
//! `session.hello` carries the hash (`HelloParams::schema`); a socketpair or
//! hosted-vsock peer whose hash does not match this build's gets
//! `error{kind: protocol}`, then the connection closes.

use std::sync::OnceLock;

use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::broker::{
    AgentCapabilities, AgentEntry, AgentEntrySkill, ProgressParams, RelayedAskParams,
    TaskRunParams, Welcome,
};
use super::error::WireError;
use super::identity::TaskIdentity;
use super::opaque::Opaque;
use super::payload::{
    HandoffInvalid, TaskMetadata, WireModelRef, WireProgress, WireUsage, WireUsageLine,
};
use super::worker::{
    HelloParams, ParticipantCard, ParticipantSkill, TaskEvent, TaskOutcome, WorkerSwarmItem,
};
use super::{IdParams, TaskState};
use crate::{
    Answer, ApprovalRequest, ApprovalVerdict, AskReply, AskRequest, Asker, HandoffContract,
    InvokeAgentOutcome, InvokeAgentParams, ItemRef, LockedPlugin, MessageStatus, ModuleCallOutcome,
    ModuleCallParams, Question, QuestionOrigin, Refusal, Remaining, SendMessageParams,
    SpawnContext,
};

/// One field per method the v3 envelope carries, named after it (ADR-0021
/// § 1's table): the four opaque payloads aside, every shape here is
/// `#[derive(JsonSchema)]`'s, never hand-written. Dead code: every field
/// exists only to be named in the generated schema.
#[derive(JsonSchema)]
#[allow(dead_code)]
struct Catalog {
    #[schemars(rename = "session.hello")]
    session_hello: HelloParams,
    #[schemars(rename = "session.hello#result")]
    session_hello_result: Welcome<'static>,
    #[schemars(rename = "agent.invoke")]
    agent_invoke: InvokeAgentParams,
    #[schemars(rename = "agent.invoke#result")]
    agent_invoke_result: InvokeAgentOutcome,
    #[schemars(rename = "agent.list#result")]
    agent_list_result: Vec<AgentEntry>,
    #[schemars(rename = "mailbox.post")]
    mailbox_post: SendMessageParams,
    #[schemars(rename = "mailbox.post#result")]
    mailbox_post_result: MessageStatus,
    #[schemars(rename = "mailbox.take#result")]
    mailbox_take_result: Vec<String>,
    #[schemars(rename = "module.call")]
    module_call: ModuleCallParams,
    #[schemars(rename = "module.call#result")]
    module_call_result: ModuleCallOutcome,
    #[schemars(rename = "human.approve")]
    human_approve: ApprovalRequest,
    #[schemars(rename = "human.approve#result")]
    human_approve_result: ApprovalVerdict,
    #[schemars(rename = "human.ask")]
    human_ask: AskRequest,
    #[schemars(rename = "human.ask#reply")]
    human_ask_reply: AskReply,
    #[schemars(rename = "human.ask#answers")]
    human_ask_answers: Vec<Answer>,
    #[schemars(rename = "task.run")]
    task_run: TaskRunParams<'static>,
    #[schemars(rename = "task.run#result")]
    task_run_result: TaskOutcome<'static>,
    #[schemars(rename = "task.event")]
    task_event: TaskEvent<'static>,
    #[schemars(rename = "req.progress")]
    req_progress: ProgressParams<'static>,
    #[schemars(rename = "req.cancel")]
    req_cancel: IdParams,
    #[schemars(rename = "broker.human_ask")]
    broker_human_ask: RelayedAskParams<'static>,
    error: WireError,
}

// Referenced only so that every wire-owned type stays attached to the
// crate's public schema surface even if `Catalog` stops reaching one of
// them; a type this crate forgets to wire in here would otherwise compile
// silently with no `JsonSchema` coverage at all.
#[allow(dead_code)]
fn assert_json_schema<T: JsonSchema>() {}
#[allow(dead_code)]
fn every_wire_type_has_a_schema() {
    assert_json_schema::<ParticipantCard>();
    assert_json_schema::<ParticipantSkill>();
    assert_json_schema::<WorkerSwarmItem>();
    assert_json_schema::<TaskState>();
    assert_json_schema::<TaskIdentity>();
    assert_json_schema::<Opaque>();
    assert_json_schema::<TaskMetadata>();
    assert_json_schema::<HandoffInvalid>();
    assert_json_schema::<WireUsage>();
    assert_json_schema::<WireUsageLine>();
    assert_json_schema::<WireModelRef>();
    assert_json_schema::<WireProgress>();
    assert_json_schema::<AgentCapabilities>();
    assert_json_schema::<AgentEntrySkill>();
    assert_json_schema::<HandoffContract>();
    assert_json_schema::<SpawnContext>();
    assert_json_schema::<Remaining>();
    assert_json_schema::<Refusal>();
    assert_json_schema::<Asker>();
    assert_json_schema::<Question>();
    assert_json_schema::<QuestionOrigin>();
    assert_json_schema::<LockedPlugin>();
    assert_json_schema::<ItemRef>();
}

/// Recursively re-sort every JSON object's entries by key, in place, and
/// drop `description` (rustdoc-derived prose, not wire shape). Arrays are
/// recursed into but left in their original order. The same canonicalizing
/// shape as `chatty_core::settings_snapshot`: independent of `HashMap`
/// iteration order or the `preserve_order` feature GPUI pulls in, so the
/// export is the same bytes whatever else got built alongside it.
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

/// Generate the canonical wire schema from the Rust types. Feature- and
/// platform-independent: nothing here reads a `cfg` or an environment
/// variable, so the same bytes come out of every build.
pub fn export() -> Value {
    let schema = SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<Catalog>();
    let mut value = schema.to_value();
    if let Value::Object(object) = &mut value {
        object.remove("title");
    }
    canonicalize(&mut value);
    value
}

/// `export()`, serialized the same way every time: keys sorted (`export`
/// already did that), two-space indent, one trailing newline.
pub fn canonical_bytes() -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(&export()).expect("the export always serializes");
    bytes.push(b'\n');
    bytes
}

/// The committed export: `crates/chatty-fabric/schema/fabric-v3.json`.
/// `tests::schema_export_is_up_to_date` is what keeps it in sync with
/// [`export`]; this is the text [`hash`] hashes.
const COMMITTED_EXPORT: &str = include_str!("../../schema/fabric-v3.json");

/// The SHA-256 (hex) of the committed export: what `session.hello` carries
/// as `schema` (ADR-0021 § 1). Computed once, from the file checked into
/// this build, not from [`export`] — so a build whose code and checked-in
/// export have drifted (caught by `schema_export_is_up_to_date` in CI) does
/// not silently hash something nobody reviewed.
pub fn hash() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| {
        let mut hasher = Sha256::new();
        hasher.update(COMMITTED_EXPORT.as_bytes());
        hex::encode(hasher.finalize())
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn golden_path() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/fabric-v3.json")
    }

    /// Step 3's done condition (ADR-0021 § 1): the committed export is
    /// exactly what the Rust types generate today. Re-run with
    /// `UPDATE_GOLDENS=1` after a deliberate wire change, and say why in
    /// the PR.
    #[test]
    fn schema_export_is_up_to_date() {
        let generated = canonical_bytes();
        let path = golden_path();
        if std::env::var("UPDATE_GOLDENS").is_ok() {
            std::fs::write(&path, &generated).unwrap();
            return;
        }
        let committed = std::fs::read(&path).unwrap_or_else(|_| {
            panic!(
                "missing schema export {}; re-run with UPDATE_GOLDENS=1",
                path.display()
            )
        });
        assert_eq!(
            String::from_utf8_lossy(&committed),
            String::from_utf8_lossy(&generated),
            "crates/chatty-fabric/schema/fabric-v3.json is stale: a wire type changed without \
             regenerating it. Re-run with UPDATE_GOLDENS=1 and say why in the PR (ADR-0021 § 1)."
        );
    }

    /// Kill criterion 4 (ADR-0021): the only any-value ("accepts anything")
    /// schemas in the export are the four payloads chatty-core owns by
    /// contract. Adding a fifth means the criterion fired — see the PR
    /// body, never widen this list to make the test pass.
    #[test]
    fn export_has_no_any_value_outside_four_opaque_payloads() {
        const ALLOWED: [&str; 4] = [
            // The captured conversation (`TaskMetadata.conversation`).
            "/$defs/TaskMetadata/properties/conversation",
            // The handoff answer (`TaskMetadata.handoff`).
            "/$defs/TaskMetadata/properties/handoff",
            // The handoff schema (`HandoffContract.schema`).
            "/$defs/HandoffContract/properties/schema",
            // The virtual agent's evidence data (`TaskMetadata.evidence`).
            "/$defs/TaskMetadata/properties/evidence",
        ];

        let mut found = Vec::new();
        fn walk(pointer: &str, value: &Value, found: &mut Vec<String>) {
            // `Opaque::json_schema` emits the unconstrained boolean schema
            // `true`; `schemars` rewrites a bare `true` to the equivalent
            // empty object `{}` wherever it has to attach a sibling key
            // (`allow_null` on an `Option<Opaque>` field, here on all four).
            // Both spellings mean the same schema ("accepts anything"); an
            // empty object elsewhere would mean a type generates no
            // constraints at all, which no real wire type here does, so
            // this stays unambiguous in this export.
            let is_any = matches!(value, Value::Bool(true))
                || matches!(value, Value::Object(map) if map.is_empty());
            if is_any {
                found.push(pointer.to_string());
            }
            match value {
                Value::Object(map) => {
                    for (key, child) in map {
                        walk(&format!("{pointer}/{key}"), child, found);
                    }
                }
                Value::Array(items) => {
                    for (i, child) in items.iter().enumerate() {
                        walk(&format!("{pointer}/{i}"), child, found);
                    }
                }
                _ => {}
            }
        }
        walk("", &export(), &mut found);
        found.sort();
        let mut allowed = ALLOWED.to_vec();
        allowed.sort_unstable();
        assert_eq!(
            found, allowed,
            "the export's any-value schemas must be exactly the four opaque payloads ADR-0021 \
             § 1 names; adding a fifth is kill criterion 4 (say so in the PR, don't widen this \
             list silently)"
        );
    }

    /// MK-2 (ADR-0024 § 7) changed the wire — `module.call`, and
    /// `needs_acceptance` and `fee_refused` errors — so a peer built before
    /// it is refused at hello rather than misreading a frame. The hash is
    /// GT-0's last, which this build must no longer carry, and the export
    /// names the new method and errors.
    #[test]
    fn schema_hash_bumped() {
        const BEFORE_MK2: &str = "ad2106a853db94d53dbac9810dae4f08397276fb9a370993177ae14b8beea323";
        assert_ne!(
            hash(),
            BEFORE_MK2,
            "the wire changed; its schema hash must too"
        );
        let export = String::from_utf8(canonical_bytes()).unwrap();
        for name in [
            "module.call",
            "module.call#result",
            "needs_acceptance",
            "fee_refused",
        ] {
            assert!(
                export.contains(&format!("\"{name}\"")),
                "the export names {name}"
            );
        }
    }

    /// The hash is of the committed file, not a moving target, and it is
    /// stable across calls (`OnceLock`).
    #[test]
    fn hash_is_a_stable_sha256_of_the_committed_export() {
        let mut hasher = Sha256::new();
        hasher.update(COMMITTED_EXPORT.as_bytes());
        let expected = hex::encode(hasher.finalize());
        assert_eq!(hash(), expected);
        assert_eq!(hash(), hash(), "memoized, not recomputed");
        assert_eq!(hash().len(), 64, "sha256 hex is 64 characters");
    }
}
