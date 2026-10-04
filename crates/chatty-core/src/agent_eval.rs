//! A spec's `[eval]` section (MK-T6, AGE-851): the small benchmark a
//! publisher attaches to a team, so its listing can show a measured score
//! rather than a claim.
//!
//! ```toml
//! [[eval.tasks]]
//! id = "hold-bad-iban"
//! prompt = "Prepare the payment run in payments.csv."
//! verify = "grep -q NL00BANK0000000000 \"$CHATTY_EVAL_ANSWER\""
//! files = { "payments.csv" = "vendor,iban,amount\n…" }
//! ```
//!
//! Each task is a prompt, the fixture files its workspace starts with, and a
//! verifier: a shell command run in that workspace after the agent is done,
//! with `CHATTY_EVAL_ANSWER` naming a file that holds the agent's final
//! answer. Exit code 0 is a pass. `chatty-tui --eval-team` runs them.
//!
//! The section is the **bundle**. [`EvalSection::bundle_sha256`] hashes it
//! the way the registry does when it signs a published version (the
//! manifest's `eval_sha256`), so a result names exactly the tasks it was
//! measured on, and a new version, whose bundle is its own, starts unscored.
//!
//! Every field is written back exactly as it was read (no defaults filled
//! in), so the hash of a parsed spec equals the hash of the document it came
//! from.

use std::collections::BTreeMap;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The most tasks one bundle may hold: it is a small benchmark, run k times.
pub const MAX_EVAL_TASKS: usize = 50;

/// `[eval]`: the tasks a team is measured on.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalSection {
    pub tasks: Vec<EvalTask>,
}

/// One `[[eval.tasks]]` entry.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalTask {
    /// Unique within the bundle; a spec name's rule.
    pub id: String,
    /// What the team is asked, as its first message.
    pub prompt: String,
    /// The fixture files the task's workspace starts with: relative path →
    /// content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<BTreeMap<String, String>>,
    /// A shell command run in the workspace after the run; exit 0 passes.
    pub verify: String,
}

impl EvalSection {
    /// Lowercase hex SHA-256 of the section's canonical JSON: what the
    /// registry signs into a published version as `eval_sha256`.
    pub fn bundle_sha256(&self) -> String {
        let value = serde_json::to_value(self).expect("an eval section serializes");
        hex::encode(Sha256::digest(hive_client::verify::canonical_json(&value)))
    }

    /// Every problem with the bundle, as sentences.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.tasks.is_empty() {
            problems.push("eval: lists no tasks".to_string());
        }
        if self.tasks.len() > MAX_EVAL_TASKS {
            problems.push(format!(
                "eval: {} tasks, at most {MAX_EVAL_TASKS}",
                self.tasks.len()
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for task in &self.tasks {
            if !is_task_id(&task.id) {
                problems.push(format!(
                    "eval: task id '{}' must be 1-64 of a-z, 0-9, '-' and '_', starting with a letter or digit",
                    task.id
                ));
            } else if !seen.insert(task.id.as_str()) {
                problems.push(format!("eval: task '{}' is listed more than once", task.id));
            }
            if task.prompt.trim().is_empty() {
                problems.push(format!("eval: task '{}' has no prompt", task.id));
            }
            if task.verify.trim().is_empty() {
                problems.push(format!("eval: task '{}' has no verify command", task.id));
            }
            for path in task.files.iter().flat_map(|files| files.keys()) {
                if !is_fixture_path(path) {
                    problems.push(format!(
                        "eval: task '{}': file '{path}' must be a relative path inside the workspace",
                        task.id
                    ));
                }
            }
        }
        problems
    }
}

fn is_task_id(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    id.len() <= 64
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// A fixture path stays inside the workspace: relative, no `..`, no root.
pub fn is_fixture_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_spec::AgentSpec;

    const SPEC: &str = r#"
[agent]
name = "payments-lead"

[[eval.tasks]]
id = "hold-bad-iban"
prompt = "Prepare the payment run in payments.csv."
verify = "grep -q NL00 \"$CHATTY_EVAL_ANSWER\""
files = { "payments.csv" = "vendor,iban\nAcme,NL00\n" }

[[eval.tasks]]
id = "all-clear"
prompt = "Anything to hold?"
verify = "true"
"#;

    #[test]
    fn eval_section_parses_and_round_trips() {
        let spec = AgentSpec::from_toml(SPEC).unwrap();
        let eval = spec.eval.as_ref().expect("the section parses");
        assert_eq!(eval.tasks.len(), 2);
        assert_eq!(
            eval.tasks[0].files.as_ref().unwrap()["payments.csv"],
            "vendor,iban\nAcme,NL00\n"
        );
        assert_eq!(eval.tasks[1].files, None, "no default is filled in");
        spec.validate(None).unwrap();
        assert_eq!(AgentSpec::from_json(&spec.to_json().unwrap()).unwrap(), spec);
        assert_eq!(AgentSpec::from_toml(&spec.to_toml().unwrap()).unwrap(), spec);

        let unknown = SPEC.replace("verify = \"true\"", "verify = \"true\"\nweight = 2");
        let err = format!("{:#}", AgentSpec::from_toml(&unknown).unwrap_err());
        assert!(err.contains("weight"), "{err}");
    }

    /// The hash is over the section's canonical JSON, the bytes the
    /// registry hashes from the published document: key order and the
    /// document's other sections do not move it; any task change does.
    #[test]
    fn bundle_hash_is_the_canonical_section() {
        let spec = AgentSpec::from_toml(SPEC).unwrap();
        let eval = spec.eval.unwrap();
        let published: serde_json::Value = serde_json::from_str(
            r#"{"tasks":[{"verify":"grep -q NL00 \"$CHATTY_EVAL_ANSWER\"","prompt":"Prepare the payment run in payments.csv.","id":"hold-bad-iban","files":{"payments.csv":"vendor,iban\nAcme,NL00\n"}},{"verify":"true","prompt":"Anything to hold?","id":"all-clear"}]}"#,
        )
        .unwrap();
        let expected = hex::encode(Sha256::digest(hive_client::verify::canonical_json(
            &published,
        )));
        assert_eq!(eval.bundle_sha256(), expected);

        let mut changed = eval.clone();
        changed.tasks[1].verify = "false".to_string();
        assert_ne!(changed.bundle_sha256(), eval.bundle_sha256());
    }

    #[test]
    fn a_bad_bundle_reports_every_problem() {
        let eval = EvalSection {
            tasks: vec![
                EvalTask {
                    id: "Bad Id".into(),
                    prompt: " ".into(),
                    verify: String::new(),
                    files: Some(BTreeMap::from([
                        ("../escape".to_string(), String::new()),
                        ("/etc/passwd".to_string(), String::new()),
                        ("ok/nested.txt".to_string(), String::new()),
                    ])),
                },
                EvalTask {
                    id: "dup".into(),
                    prompt: "p".into(),
                    verify: "true".into(),
                    files: None,
                },
                EvalTask {
                    id: "dup".into(),
                    prompt: "p".into(),
                    verify: "true".into(),
                    files: None,
                },
            ],
        };
        let all = eval.problems().join("\n");
        for expected in [
            "task id 'Bad Id'",
            "has no prompt",
            "has no verify command",
            "'../escape'",
            "'/etc/passwd'",
            "'dup' is listed more than once",
        ] {
            assert!(all.contains(expected), "{expected:?} missing from {all}");
        }
        assert!(!all.contains("ok/nested.txt"), "{all}");
        assert!(EvalSection::default().problems()[0].contains("no tasks"));
    }
}
