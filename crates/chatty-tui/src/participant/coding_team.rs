//! The coding team (AGE-957) on the swarm kit: the `coding-lead`, `coder`,
//! `tester` and `reviewer` presets run as real `chatty-tui` workers in a git
//! repository, behind a scripted fake model, and take one small issue from a
//! task to commits on the conversation's working branch in a bare remote.
//!
//! What is real: every worker process, its preset spec (profile, delegation,
//! budget), its own `git worktree`, the git tools the coder commits with, the
//! lead's `git_merge`, and the tester's shell running the repository's own
//! check (`.chatty/checks`), whose real output reaches the tester's model.
//! What is scripted: each model's replies, so the verdicts are the script's
//! words, and the lead's reading of them is too.
//!
//! The last step, pushing the result to the bare repository's working
//! branch, is the test's: no preset pushes (the coder is told never to, the
//! lead has no shell). On a hosted run the host pushes the workspace's
//! branch when the turn ends (hive AGE-915); locally the root takes the
//! lead's branch. The test does both the way they do: the root fast-forwards
//! its working branch to the lead's, and the push stands in for the host's.

use std::path::Path;

use chatty_core::agent_spec::{AgentSpec, load_agent_spec_from};
use chatty_core::testing::fake_model::{RecordedRequest, Reply, Script};

use super::swarm_kit::{AgentDef, Endpoint, SwarmKit, git};

const LEAD_MODEL: &str = "kit/coding-lead";
const CODER_MODEL: &str = "kit/coder";
const TESTER_MODEL: &str = "kit/tester";
const REVIEWER_MODEL: &str = "kit/reviewer";

/// The conversation's working branch, as hive names it (`chatty/<id>`).
const WORKING_BRANCH: &str = "chatty/conv-957";

/// The repository's own check: a failing `sub` prints what it got.
const TEST_SCRIPT: &str = r#". ./calc.sh
[ "$(add 2 3)" = 5 ] || { echo "FAIL: add 2 3 gave '$(add 2 3)', want 5"; exit 1; }
[ "$(sub 5 3)" = 2 ] || { echo "FAIL: sub 5 3 gave '$(sub 5 3)', want 2"; exit 1; }
echo "2 passed"
"#;

const CALC: &str = "add() { echo $(( $1 + $2 )); }\n";
const CALC_WRONG_SUB: &str = "add() { echo $(( $1 + $2 )); }\nsub() { echo $(( $1 + $2 )); }\n";
const SUB_WRONG: &str = "sub() { echo $(( $1 + $2 )); }";
const SUB_RIGHT: &str = "sub() { echo $(( $1 - $2 )); }";

const TASK: &str = "Issue: add sub(a, b) to calc.sh, returning a - b. \
                    Acceptance: tests/test_calc.sh passes.";

/// A compiled-in preset, never a spec from the developer's data dir.
fn preset(name: &str) -> AgentSpec {
    load_agent_spec_from(name, None, None)
        .expect("the preset loads")
        .spec
}

/// The tool names a request offered the model.
fn tools(request: &RecordedRequest) -> Vec<String> {
    request.json()["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A request's body as text, JSON string escapes for newlines and quotes
/// undone, so a tool's output can be searched for as it was printed.
fn body(request: &RecordedRequest) -> String {
    String::from_utf8_lossy(&request.body)
        .replace("\\n", "\n")
        .replace("\\\"", "\"")
}

/// The broker's edge log as `(kind, from, to, outcome)`, in the order the
/// calls ended.
fn edges(kit: &SwarmKit) -> Vec<(String, String, String, String)> {
    let log = std::fs::read_to_string(kit.broker().edge_log_path()).expect("the edge log");
    log.lines()
        .map(|line| {
            let row: serde_json::Value = serde_json::from_str(line).expect("a JSON row");
            let field = |name: &str| row[name].as_str().unwrap_or_default().to_string();
            (field("kind"), field("from"), field("to"), field("outcome"))
        })
        .collect()
}

fn edge(from: &str, to: &str) -> (String, String, String, String) {
    (
        "task".to_string(),
        from.to_string(),
        to.to_string(),
        "completed".to_string(),
    )
}

fn invoke(agent: &str, prompt: &str) -> Reply {
    Reply::tool_call(
        "invoke_agent",
        serde_json::json!({ "agent": agent, "prompt": prompt }),
    )
}

fn merge(branch: &str) -> Reply {
    Reply::tool_call("git_merge", serde_json::json!({ "branch": branch }))
}

/// One tester run: read the checks, run the one there is, give a verdict.
fn tester_run(verdict: &str) -> [Reply; 3] {
    [
        Reply::tool_call("read_file", serde_json::json!({ "path": ".chatty/checks" })),
        Reply::tool_call(
            "shell_execute",
            serde_json::json!({ "command": "sh tests/test_calc.sh" }),
        ),
        Reply::text(verdict),
    ]
}

/// The repository the issue is about, committed on the working branch and
/// pushed to a bare remote, as hive's checkout leaves a conversation's
/// workspace (AGE-915).
fn seed_repository(workspace: &Path, remote: &Path) {
    std::fs::write(workspace.join("calc.sh"), CALC).unwrap();
    std::fs::create_dir_all(workspace.join("tests")).unwrap();
    std::fs::write(workspace.join("tests/test_calc.sh"), TEST_SCRIPT).unwrap();
    std::fs::create_dir_all(workspace.join(".chatty")).unwrap();
    std::fs::write(
        workspace.join(".chatty/checks"),
        "# The repository's checks, one shell command per line.\nsh tests/test_calc.sh\n",
    )
    .unwrap();
    git(
        workspace,
        &["add", "calc.sh", "tests/test_calc.sh", ".chatty/checks"],
    );
    git(
        workspace,
        &["commit", "-q", "-m", "calc with add, and its test"],
    );
    git(
        remote.parent().unwrap(),
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            remote.to_str().unwrap(),
        ],
    );
    git(
        workspace,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(workspace, &["push", "-q", "origin", "main"]);
    git(workspace, &["switch", "-q", "-c", WORKING_BRANCH]);
    git(workspace, &["push", "-q", "-u", "origin", WORKING_BRANCH]);
}

/// AGE-957's scripted run. The lead hands the issue to the coder, which
/// commits a wrong `sub` on its own branch; the lead merges it, and the
/// tester runs the repository's check, which really fails, and answers
/// `TESTS: FAIL` with that output. The lead sends the failure back as a
/// numbered fix; the coder fixes and commits; merged and tested again, the
/// check really passes, the tester answers `TESTS: PASS`, the reviewer
/// `APPROVE`, and the lead `DONE`. The lead's branch then goes to the bare
/// repository's working branch, which ends up with both commits and a
/// passing check, while its base branch is untouched.
#[tokio::test]
async fn coding_team_takes_an_issue_to_the_working_branch() {
    let lead = preset("coding-lead");
    let kit = SwarmKit::start_in_repo(
        vec![
            AgentDef::from_spec(lead, LEAD_MODEL, Endpoint::Sse),
            AgentDef::from_spec(preset("coder"), CODER_MODEL, Endpoint::Ndjson),
            AgentDef::from_spec(preset("tester"), TESTER_MODEL, Endpoint::Ndjson),
            AgentDef::from_spec(preset("reviewer"), REVIEWER_MODEL, Endpoint::Ndjson),
        ],
        Script::new().route(
            LEAD_MODEL,
            [
                invoke("coder", TASK),
                merge("sub-agent/coder-0"),
                invoke("tester", "Run the repository's checks."),
                invoke(
                    "coder",
                    "Fixes:\n1. FAIL: sub 5 3 gave '8', want 2 (tests/test_calc.sh). \
                     sub must return a - b.",
                ),
                merge("sub-agent/coder-1"),
                invoke("tester", "Run the repository's checks."),
                invoke(
                    "reviewer",
                    "Review the newest commits on the current branch against: \
                     sub(a, b) returns a - b; tests/test_calc.sh passes.",
                ),
                Reply::text("DONE\nsub() added and fixed; TESTS: PASS; APPROVE."),
            ],
        ),
        Script::new()
            .route(
                CODER_MODEL,
                [
                    // Round 1: a wrong sub, committed on its branch.
                    Reply::tool_call("read_file", serde_json::json!({ "path": "calc.sh" })),
                    Reply::tool_call(
                        "write_file",
                        serde_json::json!({ "path": "calc.sh", "content": CALC_WRONG_SUB }),
                    ),
                    Reply::tool_call("git_add", serde_json::json!({ "paths": ["calc.sh"] })),
                    Reply::tool_call(
                        "git_commit",
                        serde_json::json!({ "message": "Add sub() to calc.sh" }),
                    ),
                    Reply::text("Committed \"Add sub() to calc.sh\"; changed calc.sh."),
                    // Round 2: the numbered fix.
                    Reply::tool_call(
                        "apply_diff",
                        serde_json::json!({
                            "path": "calc.sh",
                            "old_content": SUB_WRONG,
                            "new_content": SUB_RIGHT,
                        }),
                    ),
                    Reply::tool_call("git_add", serde_json::json!({ "paths": ["calc.sh"] })),
                    Reply::tool_call(
                        "git_commit",
                        serde_json::json!({ "message": "Fix sub(): subtract b from a" }),
                    ),
                    Reply::text("1. sub now returns a - b. Committed \"Fix sub()\"."),
                ],
            )
            .route(TESTER_MODEL, {
                let mut replies = tester_run(
                    "TESTS: FAIL\nsh tests/test_calc.sh: exit 1\nFAIL: sub 5 3 gave '8', want 2",
                )
                .to_vec();
                replies.extend(tester_run("TESTS: PASS\nsh tests/test_calc.sh: exit 0"));
                replies
            })
            .route(
                REVIEWER_MODEL,
                [
                    Reply::tool_call("git_log", serde_json::json!({ "max_count": 3 })),
                    Reply::text("APPROVE\n1. sub(a, b) returns a - b.\n2. The test passes."),
                ],
            ),
    )
    .await;

    let workspace = kit.workspace();
    let remote = kit.root().join("remote.git");
    seed_repository(&workspace, &remote);
    let base = git(&workspace, &["rev-parse", "main"]);

    let run = kit.run_leader(TASK).await;
    let out = run.output.as_ref().expect("the team run succeeded");
    assert!(out.success, "{out:?}");
    assert_eq!(
        out.response.lines().next(),
        Some("DONE"),
        "{}",
        out.response
    );

    // Who called whom: the lead, and nobody else, delegated; the coder and
    // the tester twice each, the reviewer once.
    let lead_node = "coding-lead-0";
    assert_eq!(
        edges(&kit),
        [
            edge(lead_node, "coder-0"),
            edge(lead_node, "tester-0"),
            edge(lead_node, "coder-1"),
            edge(lead_node, "tester-1"),
            edge(lead_node, "reviewer-0"),
            edge("root", lead_node),
        ]
    );

    // The roles' tools: the lead delegates and merges but edits nothing;
    // the coder edits and commits and delegates to nobody; the tester runs
    // a shell and can write nothing.
    let lead_requests = kit.sse.requests_for(LEAD_MODEL);
    assert_eq!(lead_requests.len(), 8);
    let lead_tools = tools(&lead_requests[0]);
    for tool in ["invoke_agent", "git_merge"] {
        assert!(
            lead_tools.iter().any(|t| t == tool),
            "{tool}: {lead_tools:?}"
        );
    }
    for tool in ["write_file", "apply_diff", "git_commit", "shell_execute"] {
        assert!(
            !lead_tools.iter().any(|t| t == tool),
            "{tool}: {lead_tools:?}"
        );
    }
    let coder_requests = kit.ndjson.requests_for(CODER_MODEL);
    assert_eq!(
        coder_requests.len(),
        9,
        "five calls in round 1, four in round 2"
    );
    let coder_tools = tools(&coder_requests[0]);
    for tool in ["apply_diff", "write_file", "git_commit", "shell_execute"] {
        assert!(
            coder_tools.iter().any(|t| t == tool),
            "{tool}: {coder_tools:?}"
        );
    }
    for tool in ["invoke_agent", "execute_code"] {
        assert!(
            !coder_tools.iter().any(|t| t == tool),
            "{tool}: {coder_tools:?}"
        );
    }
    let tester_requests = kit.ndjson.requests_for(TESTER_MODEL);
    assert_eq!(tester_requests.len(), 6);
    let tester_tools = tools(&tester_requests[0]);
    assert!(
        tester_tools.iter().any(|t| t == "shell_execute"),
        "{tester_tools:?}"
    );
    for tool in ["write_file", "apply_diff", "git_commit", "invoke_agent"] {
        assert!(
            !tester_tools.iter().any(|t| t == tool),
            "{tool}: {tester_tools:?}"
        );
    }

    // The tester read the checks from the repository and ran them in a tree
    // that holds the coder's merged commit: the first run's real output is
    // the wrong sub's failure, the second's the pass.
    assert!(
        body(&tester_requests[1]).contains("sh tests/test_calc.sh"),
        "the checks came from .chatty/checks"
    );
    assert!(
        body(&tester_requests[2]).contains("FAIL: sub 5 3 gave '8', want 2"),
        "the first check really failed: {}",
        body(&tester_requests[2])
    );
    assert!(
        body(&tester_requests[5]).contains("2 passed"),
        "the second check really passed: {}",
        body(&tester_requests[5])
    );

    // The lead acted on each verdict: the failure reached it before it sent
    // the fix, and the pass and the approval before it said DONE.
    assert!(body(&lead_requests[3]).contains("TESTS: FAIL"));
    assert!(body(&coder_requests[5]).contains("1. FAIL: sub 5 3 gave '8', want 2"));
    assert!(body(&lead_requests[6]).contains("TESTS: PASS"));
    assert!(body(&lead_requests[7]).contains("APPROVE"));

    // The lead's branch holds both of the coder's commits on top of the
    // base, and the base branch was never moved.
    let lead_branch = format!("sub-agent/{lead_node}");
    assert_eq!(
        git(
            &workspace,
            &[
                "log",
                "--format=%s",
                &format!("{WORKING_BRANCH}..{lead_branch}")
            ]
        ),
        "Fix sub(): subtract b from a\nAdd sub() to calc.sh"
    );
    assert_eq!(git(&workspace, &["rev-parse", "main"]), base);

    // The root takes the lead's branch and the result is pushed to the
    // working branch, as the host does at the turn's end.
    git(&workspace, &["merge", "-q", "--ff-only", &lead_branch]);
    git(&workspace, &["push", "-q", "origin", WORKING_BRANCH]);

    // In the bare repository: the working branch carries the fix, and the
    // check passes on a fresh clone of it; main is where it was.
    assert_eq!(
        git(&remote, &["log", "--format=%s", "-2", WORKING_BRANCH]),
        "Fix sub(): subtract b from a\nAdd sub() to calc.sh"
    );
    assert_eq!(git(&remote, &["rev-parse", "main"]), base);
    let clone = kit.root().join("clone");
    git(
        &kit.root(),
        &[
            "clone",
            "-q",
            "-b",
            WORKING_BRANCH,
            remote.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    let check = std::process::Command::new("sh")
        .arg("tests/test_calc.sh")
        .current_dir(&clone)
        .output()
        .expect("sh runs");
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stdout)
    );
    assert_eq!(String::from_utf8_lossy(&check.stdout).trim(), "2 passed");
}
