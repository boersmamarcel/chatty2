//! Typed handoffs between a team's roles (TD-2, AGE-693).
//!
//! A team file can name a JSON Schema per role (`team.json` `handoffs`,
//! fabric-team-design §3). A worker running as that role must end its final
//! answer with one fenced `json` block matching the schema:
//!
//! - **valid:** the parsed JSON rides the worker's terminal status under
//!   [`HANDOFF_METADATA_KEY`], and the caller's `invoke_agent` result carries
//!   it as `handoff`;
//! - **invalid, the first time:** the worker gets one follow-up turn naming
//!   the schema errors ([`follow_up`]);
//! - **invalid again:** the task fails with the errors under
//!   [`HANDOFF_INVALID_METADATA_KEY`], which the caller's `invoke_agent`
//!   turns into `InvokeAgentError::HandoffInvalid`.
//!
//! A schema may also carry `x-must-be-read` read rules: which fields of an
//! earlier role's handoff this role must quote. The leader checks them on
//! its side ([`HandoffLedger`]) and records a violation as the
//! `handoff_misread` failure tag, with no retry. The ledger also counts the
//! invalid handoffs per role; both are what TD-1's scorecard reads, from
//! the leader's `--usage-file`.

use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value;
use tracing::warn;

pub use chatty_fabric::HandoffContract;

/// The terminal-status metadata key a valid handoff rides under.
pub const HANDOFF_METADATA_KEY: &str = "handoff";

/// The terminal-status metadata key a failed handoff's role and schema
/// errors ride under: `{"role": …, "errors": [...]}`.
pub const HANDOFF_INVALID_METADATA_KEY: &str = "handoffInvalid";

/// The terminal-status metadata key for how many of the worker's answers
/// failed their schema, the last one included. Absent when none did.
pub const HANDOFF_INVALID_COUNT_METADATA_KEY: &str = "handoffInvalidCount";

/// The schema keyword naming what a role must quote from an earlier role's
/// handoff: `{"x-must-be-read": {"<role>": ["<field>", ...]}}`.
pub const MUST_BE_READ_KEYWORD: &str = "x-must-be-read";

/// The failure tag a violated read rule is recorded as (TD-1).
pub const HANDOFF_MISREAD_TAG: &str = "handoff_misread";

/// How the one handoff re-prompt starts, so the worker's frame mapper can
/// count it among the invalid answers.
pub const HANDOFF_FOLLOW_UP_PREFIX: &str = "Your handoff does not match its schema";

/// How a worker's final answer measured up to its role's schema.
#[derive(Debug, Clone, PartialEq)]
pub enum HandoffOutcome {
    /// The answer's one `json` block, parsed, matching the schema.
    Valid(Value),
    /// What is wrong with it, one line per error.
    Invalid { errors: Vec<String> },
}

/// Compile `schema`, which is also what checks it is a JSON Schema at all.
/// References are resolved within the document only: a remote `$ref` fails
/// here rather than fetching anything.
pub fn compile(schema: &Value) -> Result<jsonschema::Validator, String> {
    jsonschema::validator_for(schema).map_err(|e| e.to_string())
}

/// The read rules a schema declares under [`MUST_BE_READ_KEYWORD`]: earlier
/// role → the fields of its handoff this role must quote. Empty when the
/// schema declares none; an error when the keyword is not that shape.
pub fn read_rules(schema: &Value) -> Result<BTreeMap<String, Vec<String>>, String> {
    let Some(rules) = schema.get(MUST_BE_READ_KEYWORD) else {
        return Ok(BTreeMap::new());
    };
    let shape = || {
        format!(
            "`{MUST_BE_READ_KEYWORD}` must map a role to the list of its handoff's fields \
             to quote, e.g. {{\"coder\": [\"files_changed\"]}}"
        )
    };
    let Value::Object(rules) = rules else {
        return Err(shape());
    };
    rules
        .iter()
        .map(|(role, fields)| {
            let fields = fields
                .as_array()
                .and_then(|fields| {
                    fields
                        .iter()
                        .map(|f| f.as_str().map(str::to_string))
                        .collect::<Option<Vec<_>>>()
                })
                .ok_or_else(shape)?;
            Ok((role.clone(), fields))
        })
        .collect()
}

/// The fenced `json` blocks in `text`, in order: a line opening with
/// ```` ```json ```` up to the next line that is only a fence.
fn json_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut open: Option<Vec<&str>> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        match open.as_mut() {
            None => {
                if trimmed
                    .strip_prefix("```")
                    .is_some_and(|tag| tag.trim().eq_ignore_ascii_case("json"))
                {
                    open = Some(Vec::new());
                }
            }
            Some(body) => {
                if trimmed == "```" {
                    blocks.push(body.join("\n"));
                    open = None;
                } else {
                    body.push(line);
                }
            }
        }
    }
    blocks
}

/// Check `answer` — the text of the worker's last model call — against
/// `contract`'s schema.
pub fn check(contract: &HandoffContract, answer: &str) -> HandoffOutcome {
    let invalid = |error: String| HandoffOutcome::Invalid {
        errors: vec![error],
    };
    let blocks = json_blocks(answer);
    let block = match blocks.as_slice() {
        [block] => block,
        [] => {
            return invalid(
                "the final answer has no fenced ```json block; it must end with exactly one"
                    .to_string(),
            );
        }
        more => {
            return invalid(format!(
                "the final answer has {} fenced ```json blocks; it must have exactly one",
                more.len()
            ));
        }
    };
    let value: Value = match serde_json::from_str(block) {
        Ok(value) => value,
        Err(e) => return invalid(format!("the ```json block is not JSON: {e}")),
    };
    let validator = match compile(&contract.schema) {
        Ok(validator) => validator,
        Err(e) => return invalid(format!("the role's schema does not compile: {e}")),
    };
    let errors: Vec<String> = validator
        .iter_errors(&value)
        .map(|error| {
            let path = error.instance_path().to_string();
            let at = if path.is_empty() { "/" } else { &path };
            format!("{at}: {error}")
        })
        .collect();
    if errors.is_empty() {
        HandoffOutcome::Valid(value)
    } else {
        HandoffOutcome::Invalid { errors }
    }
}

/// What a worker running as `contract.role` is told along with its task.
pub fn instruction(contract: &HandoffContract) -> String {
    let schema =
        serde_json::to_string_pretty(&contract.schema).unwrap_or_else(|_| "{}".to_string());
    format!(
        "You are handing off as the `{}` role. End your final answer with exactly one \
         fenced ```json block, your handoff, matching this JSON Schema:\n\n{schema}",
        contract.role
    )
}

/// The one follow-up turn a worker gets when its handoff failed its schema.
pub fn follow_up(contract: &HandoffContract, errors: &[String]) -> String {
    let list: String = errors.iter().map(|e| format!("\n- {e}")).collect();
    format!(
        "{HANDOFF_FOLLOW_UP_PREFIX} (role `{}`):{list}\n\nAnswer again: end your final \
         answer with exactly one fenced ```json block that matches the schema. This is the \
         only retry.",
        contract.role
    )
}

/// Whether `prompt` is a handoff re-prompt.
pub fn is_follow_up(prompt: &str) -> bool {
    prompt.starts_with(HANDOFF_FOLLOW_UP_PREFIX)
}

/// What a leader's `invoke_agent` reads off a worker's terminal status.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HandoffReport {
    /// The valid handoff, when there is one.
    pub handoff: Option<Value>,
    /// The role and errors of a handoff that failed twice.
    pub invalid: Option<(String, Vec<String>)>,
    /// How many of the worker's answers failed their schema.
    pub invalid_count: u32,
}

impl HandoffReport {
    /// Read the handoff keys off a terminal status's metadata. All empty for
    /// a worker that had no contract.
    pub fn from_status_metadata(metadata: Option<&Value>) -> Self {
        let Some(metadata) = metadata else {
            return Self::default();
        };
        let invalid = metadata.get(HANDOFF_INVALID_METADATA_KEY).and_then(|v| {
            let role = v.get("role")?.as_str()?.to_string();
            let errors = v
                .get("errors")?
                .as_array()?
                .iter()
                .filter_map(|e| e.as_str().map(str::to_string))
                .collect();
            Some((role, errors))
        });
        Self {
            handoff: metadata.get(HANDOFF_METADATA_KEY).cloned(),
            invalid,
            invalid_count: metadata
                .get(HANDOFF_INVALID_COUNT_METADATA_KEY)
                .and_then(Value::as_u64)
                .map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX)),
        }
    }
}

/// The leader's record of its roles' handoffs over one run: the invalid
/// count per role and whether a read rule was violated. Shared by the
/// leader's `invoke_agent` (which writes it) and whatever reports the run
/// (chatty-tui's `--usage-file`, TD-1's scorecard). Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct HandoffLedger {
    inner: Arc<Mutex<Ledger>>,
}

#[derive(Debug, Default)]
struct Ledger {
    /// Role → (earlier role → fields to quote).
    rules: BTreeMap<String, BTreeMap<String, Vec<String>>>,
    /// Each role's latest valid handoff.
    last: BTreeMap<String, Value>,
    invalid_by_role: BTreeMap<String, u32>,
    misread: bool,
}

impl HandoffLedger {
    /// A ledger checking the read rules `contracts` declare. A rule that
    /// does not parse was refused when the team loaded, so here it is
    /// skipped.
    pub fn new<'a>(contracts: impl IntoIterator<Item = &'a HandoffContract>) -> Self {
        let rules = contracts
            .into_iter()
            .filter_map(|c| {
                let rules = read_rules(&c.schema).ok()?;
                (!rules.is_empty()).then(|| (c.role.clone(), rules))
            })
            .collect();
        Self {
            inner: Arc::new(Mutex::new(Ledger {
                rules,
                ..Ledger::default()
            })),
        }
    }

    /// Record how a delegation to `role` ended: how many of its answers
    /// failed their schema, and its handoff when it was valid, which is
    /// checked against the role's read rules.
    pub fn record(&self, role: &str, invalid_count: u32, handoff: Option<&Value>) {
        let mut ledger = self.inner.lock();
        if invalid_count > 0 {
            *ledger.invalid_by_role.entry(role.to_string()).or_default() += invalid_count;
        }
        let Some(handoff) = handoff else {
            return;
        };
        let quoted = strings(handoff).join("\n");
        let missed: Vec<String> = ledger
            .rules
            .get(role)
            .into_iter()
            .flatten()
            .flat_map(|(earlier, fields)| {
                let earlier_handoff = ledger.last.get(earlier);
                fields.iter().flat_map(move |field| {
                    earlier_handoff
                        .and_then(|h| h.get(field))
                        .map(strings)
                        .unwrap_or_default()
                        .into_iter()
                        .map(move |s| (earlier, field, s))
                })
            })
            .filter(|(_, _, s)| !quoted.contains(s.as_str()))
            .map(|(earlier, field, s)| format!("{earlier}.{field}: {s}"))
            .collect();
        if !missed.is_empty() {
            warn!(
                role,
                ?missed,
                "A handoff does not quote what its read rules require"
            );
            ledger.misread = true;
        }
        ledger.last.insert(role.to_string(), handoff.clone());
    }

    /// How many invalid handoffs each role produced (`handoff_invalid_by_role`).
    pub fn invalid_by_role(&self) -> BTreeMap<String, u32> {
        self.inner.lock().invalid_by_role.clone()
    }

    /// The failure tags the handoffs earned: [`HANDOFF_MISREAD_TAG`] or none.
    pub fn failure_tags(&self) -> Vec<String> {
        if self.inner.lock().misread {
            vec![HANDOFF_MISREAD_TAG.to_string()]
        } else {
            Vec::new()
        }
    }
}

/// Every string and number in `value`, as text.
fn strings(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) => vec![s.clone()],
        Value::Number(n) => vec![n.to_string()],
        Value::Array(items) => items.iter().flat_map(strings).collect(),
        Value::Object(map) => map.values().flat_map(strings).collect(),
        Value::Bool(_) | Value::Null => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn contract() -> HandoffContract {
        HandoffContract {
            role: "coder".to_string(),
            schema: json!({
                "type": "object",
                "required": ["files_changed"],
                "properties": {
                    "files_changed": { "type": "array", "items": { "type": "string" } }
                }
            }),
        }
    }

    #[test]
    fn one_matching_block_is_valid() {
        let answer = "Done.\n\n```json\n{\"files_changed\": [\"a.rs\"]}\n```\n";
        assert_eq!(
            check(&contract(), answer),
            HandoffOutcome::Valid(json!({ "files_changed": ["a.rs"] }))
        );
    }

    #[test]
    fn a_missing_extra_or_wrong_block_is_invalid_with_a_reason() {
        let errors = |answer: &str| match check(&contract(), answer) {
            HandoffOutcome::Invalid { errors } => errors.join(" | "),
            HandoffOutcome::Valid(v) => panic!("valid: {v}"),
        };
        assert!(errors("no block").contains("no fenced"));
        assert!(errors("```json\n{}\n```\n```json\n{}\n```").contains("2 fenced"));
        assert!(errors("```json\n{nope\n```").contains("not JSON"));
        let schema = errors("```json\n{\"files_changed\": \"a.rs\"}\n```");
        assert!(schema.contains("/files_changed"), "{schema}");
    }

    #[test]
    fn read_rules_parse_and_refuse_a_bad_shape() {
        let rules = read_rules(&json!({ "x-must-be-read": { "coder": ["files_changed"] } }));
        assert_eq!(rules.unwrap()["coder"], ["files_changed"]);
        assert!(read_rules(&json!({ "x-must-be-read": ["files_changed"] })).is_err());
        assert!(read_rules(&json!({})).unwrap().is_empty());
    }

    /// A reviewer whose handoff leaves out a file the coder changed earns
    /// `handoff_misread`; one that quotes every file does not. Invalid
    /// counts add up per role.
    #[test]
    fn the_ledger_tags_a_misread_and_counts_invalid_handoffs() {
        let reviewer = HandoffContract {
            role: "reviewer".to_string(),
            schema: json!({ "x-must-be-read": { "coder": ["files_changed"] } }),
        };
        let ledger = HandoffLedger::new([&contract(), &reviewer]);
        ledger.record(
            "coder",
            1,
            Some(&json!({ "files_changed": ["a.rs", "b.rs"] })),
        );
        ledger.record(
            "reviewer",
            0,
            Some(&json!({ "reviewed": ["a.rs", "b.rs"] })),
        );
        assert!(ledger.failure_tags().is_empty());
        ledger.record("coder", 1, None);
        assert_eq!(ledger.invalid_by_role()["coder"], 2);

        ledger.record("reviewer", 0, Some(&json!({ "reviewed": ["a.rs"] })));
        assert_eq!(ledger.failure_tags(), [HANDOFF_MISREAD_TAG]);
    }
}
