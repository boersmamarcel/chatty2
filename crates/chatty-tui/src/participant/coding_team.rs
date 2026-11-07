//! The coding team (AGE-957) on the swarm kit: the `coding-lead`, `coder`,
//! `tester` and `code-reviewer` presets run as real `chatty-tui` workers in a git
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
const REVIEWER_MODEL: &str = "kit/code-reviewer";
const GITHUB_MODEL: &str = "kit/github";

/// The conversation's working branch, as hive names it (`chatty/<id>`).
const WORKING_BRANCH: &str = "chatty/conv-957";

/// The repository's own check: a failing `sub` prints what it got.
const TEST_SCRIPT: &str = r#". ./calc.sh
[ "$(add 2 3)" = 5 ] || { echo "FAIL: add 2 3 gave '$(add 2 3)', want 5"; exit 1; }
[ "$(sub 5 3)" = 2 ] || { echo "FAIL: sub 5 3 gave '$(sub 5 3)', want 2"; exit 1; }
echo "test result: ok. 2 passed; 0 failed"
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

/// One mutation check (AGE-1032): break the line under test in the
/// working tree, run the test and see it fail, restore the file with
/// `git checkout -- calc.sh`, and show that nothing is left changed.
fn tester_mutation_run(verdict: &str) -> [Reply; 3] {
    [
        Reply::tool_call(
            "shell_execute",
            serde_json::json!({
                "command": "sed -i 's/\\$1 - \\$2/\\$1 + \\$2/' calc.sh; \\\n\\
                            sh tests/test_calc.sh; echo \"test exit $?\"; \\\n\\
                            git checkout -- calc.sh"
            }),
        ),
        Reply::tool_call(
            "shell_execute",
            serde_json::json!({ "command": "git status --short; echo tree-restored" }),
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
/// check really passes, the tester answers `TESTS: PASS`, the code-reviewer
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
            AgentDef::from_spec(preset("code-reviewer"), REVIEWER_MODEL, Endpoint::Ndjson),
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
                    "tester",
                    "Mutation check: break the line `sub() { echo $(( $1 - $2 )); }` \
                     in calc.sh (change - to +) for test tests/test_calc.sh; expect it to \
                     FAIL, restore with git checkout -- calc.sh, commit nothing.",
                ),
                invoke(
                    "code-reviewer",
                    "Review the newest commits on the current branch against: \
                     sub(a, b) returns a - b; tests/test_calc.sh passes.",
                ),
                Reply::text("DONE\nsub() added and fixed; TESTS: PASS; MUTATION CAUGHT; APPROVE."),
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
                replies.extend(tester_run(
                    "TESTS: PASS\nsh tests/test_calc.sh: exit 0\n\
                     test result: ok. 2 passed; 0 failed\nFiles changed: calc.sh",
                ));
                replies.extend(tester_mutation_run(
                    "MUTATION CAUGHT: tests/test_calc.sh, calc.sh sub(): - to +",
                ));
                replies
            })
            .route(
                REVIEWER_MODEL,
                [
                    Reply::tool_call("git_log", serde_json::json!({ "max_count": 3 })),
                    Reply::tool_call(
                        "shell_execute",
                        serde_json::json!({ "command": "git show --stat HEAD" }),
                    ),
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
            edge(lead_node, "tester-2"),
            edge(lead_node, "code-reviewer-0"),
            edge("root", lead_node),
        ]
    );

    // The roles' tools: the lead delegates and merges but edits nothing;
    // the coder edits and commits and delegates to nobody; the tester runs
    // a shell and can write nothing.
    let lead_requests = kit.sse.requests_for(LEAD_MODEL);
    assert_eq!(lead_requests.len(), 9);
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
    assert_eq!(tester_requests.len(), 9);
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
        body(&tester_requests[5]).contains("test result: ok. 2 passed"),
        "the second check really passed: {}",
        body(&tester_requests[5])
    );

    // The mutation check really broke the line and the test really failed
    // on it, then the file was restored: the tester's second shell call
    // sees a clean tree.
    assert!(
        body(&tester_requests[7]).contains("FAIL: sub 5 3 gave '8', want 2")
            && body(&tester_requests[7]).contains("test exit 1"),
        "the broken line made the test fail: {}",
        body(&tester_requests[7])
    );
    assert!(
        body(&tester_requests[8]).contains("tree-restored")
            && !body(&tester_requests[8]).contains(" M calc.sh"),
        "the file was restored: {}",
        body(&tester_requests[8])
    );

    // The reviewer has the shell now (AGE-1032): it listed the commit's
    // files with git show and the output reached it.
    let reviewer_requests = kit.ndjson.requests_for(REVIEWER_MODEL);
    assert_eq!(reviewer_requests.len(), 3);
    let reviewer_tools = tools(&reviewer_requests[0]);
    assert!(
        reviewer_tools.iter().any(|t| t == "shell_execute"),
        "{reviewer_tools:?}"
    );
    for tool in ["write_file", "apply_diff", "git_commit"] {
        assert!(
            !reviewer_tools.iter().any(|t| t == tool),
            "{tool}: {reviewer_tools:?}"
        );
    }
    assert!(body(&reviewer_requests[2]).contains("calc.sh"));

    // The lead acted on each verdict: the failure reached it before it sent
    // the fix, and the pass and the approval before it said DONE.
    assert!(body(&lead_requests[3]).contains("TESTS: FAIL"));
    assert!(body(&coder_requests[5]).contains("1. FAIL: sub 5 3 gave '8', want 2"));
    assert!(body(&lead_requests[6]).contains("TESTS: PASS"));
    assert!(body(&lead_requests[7]).contains("MUTATION CAUGHT"));
    assert!(body(&lead_requests[8]).contains("APPROVE"));

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
    assert_eq!(
        String::from_utf8_lossy(&check.stdout).trim(),
        "test result: ok. 2 passed; 0 failed"
    );
}

// AGE-1034: the CI loop. `github` is a host-side agent on a hosted run, so
// here it is a test double: a bare agent whose scripted replies are what the
// host's `open_or_update_pr`, `pr_checks` and `failing_check_log` answer.
// The coder, tester, reviewer and lead are the same presets as above.

const CALC_WITH_SUB: &str = "add() { echo $(( $1 + $2 )); }\nsub() { echo $(( $1 - $2 )); }\n";
const PR_OPENED: &str = "PR: https://github.com/acme/calc/pull/7 (draft, opened)";
const PR_UPDATED: &str = "PR: https://github.com/acme/calc/pull/7 (draft, updated)";
const REVIEW_PROMPT: &str = "Review the newest commits on the current branch against: \
                             sub(a, b) returns a - b; tests/test_calc.sh passes.";

fn open_pr(labels: &[&str]) -> Reply {
    let mut call = serde_json::json!({
        "tool": "open_or_update_pr",
        "title": "AGE-1: add sub() to calc.sh",
        "body": "Adds sub(a, b). TESTS: PASS; APPROVE. Closes AGE-1",
        "draft": true,
    });
    if !labels.is_empty() {
        call["labels"] = serde_json::json!(labels);
    }
    invoke("github", &call.to_string())
}

fn pr_checks() -> Reply {
    invoke(
        "github",
        &serde_json::json!({"tool": "pr_checks", "wait_secs": 1200}).to_string(),
    )
}

fn check_log(check: &str) -> Reply {
    invoke(
        "github",
        &serde_json::json!({"tool": "failing_check_log", "check": check, "max_kb": 16}).to_string(),
    )
}

/// The lead's first half, up to an approved change: the coder commits a
/// right `sub`, the lead merges it, the tester and the reviewer give their
/// verdicts.
fn lead_to_approval() -> Vec<Reply> {
    vec![
        invoke("coder", TASK),
        merge("sub-agent/coder-0"),
        invoke("tester", "Run the repository's checks."),
        invoke("code-reviewer", REVIEW_PROMPT),
    ]
}

/// The coder's first round: `sub` written, added and committed.
fn coder_adds_sub() -> Vec<Reply> {
    vec![
        Reply::tool_call("read_file", serde_json::json!({ "path": "calc.sh" })),
        Reply::tool_call(
            "write_file",
            serde_json::json!({ "path": "calc.sh", "content": CALC_WITH_SUB }),
        ),
        Reply::tool_call("git_add", serde_json::json!({ "paths": ["calc.sh"] })),
        Reply::tool_call(
            "git_commit",
            serde_json::json!({ "message": "Add sub() to calc.sh" }),
        ),
        Reply::text("Committed \"Add sub() to calc.sh\"; changed calc.sh."),
    ]
}

fn tester_passes() -> [Reply; 3] {
    tester_run("TESTS: PASS\nsh tests/test_calc.sh: exit 0\ntest result: ok. 2 passed; 0 failed")
}

fn reviewer_approves() -> [Reply; 3] {
    [
        Reply::tool_call("git_log", serde_json::json!({ "max_count": 3 })),
        Reply::tool_call(
            "shell_execute",
            serde_json::json!({ "command": "git show --stat HEAD" }),
        ),
        Reply::text("APPROVE\n1. sub(a, b) returns a - b.\n2. The test passes."),
    ]
}

/// A kit with the four presets and a scripted `github`.
async fn ci_kit(lead: Vec<Reply>, coder: Vec<Reply>, testers: usize, github: &[&str]) -> SwarmKit {
    let mut tester = Vec::new();
    let mut reviewer = Vec::new();
    for _ in 0..testers {
        tester.extend(tester_passes());
        reviewer.extend(reviewer_approves());
    }
    let github: Vec<Reply> = github.iter().map(|reply| Reply::text(*reply)).collect();
    let kit = SwarmKit::start_in_repo(
        vec![
            AgentDef::from_spec(preset("coding-lead"), LEAD_MODEL, Endpoint::Sse),
            AgentDef::from_spec(preset("coder"), CODER_MODEL, Endpoint::Ndjson),
            AgentDef::from_spec(preset("tester"), TESTER_MODEL, Endpoint::Ndjson),
            AgentDef::from_spec(preset("code-reviewer"), REVIEWER_MODEL, Endpoint::Ndjson),
            AgentDef::new("github", GITHUB_MODEL, Endpoint::Ndjson),
        ],
        Script::new().route(LEAD_MODEL, lead),
        Script::new()
            .route(CODER_MODEL, coder)
            .route(TESTER_MODEL, tester)
            .route(REVIEWER_MODEL, reviewer)
            .route(GITHUB_MODEL, github),
    )
    .await;
    let remote = kit.root().join("remote.git");
    seed_repository(&kit.workspace(), &remote);
    kit
}

/// What the lead sent `github`, in order, as the fake model saw it.
fn github_prompts(kit: &SwarmKit) -> Vec<String> {
    kit.ndjson
        .requests_for(GITHUB_MODEL)
        .iter()
        .map(body)
        .collect()
}

fn names(kit: &SwarmKit) -> Vec<String> {
    edges(kit).into_iter().map(|(_, _, to, _)| to).collect()
}

/// Green on the first try: the lead opens a draft PR with its verdicts,
/// asks for the checks, sees `CHECKS: PASS` and replies DONE with the link.
#[tokio::test]
async fn ci_loop_green_on_the_first_try() {
    let kit = ci_kit(
        {
            let mut lead = lead_to_approval();
            lead.extend([
                open_pr(&[]),
                pr_checks(),
                Reply::text(format!(
                    "DONE\n{PR_OPENED}\nCHECKS: PASS\nTESTS: PASS; APPROVE"
                )),
            ]);
            lead
        },
        coder_adds_sub(),
        1,
        &[PR_OPENED, "CHECKS: PASS\ntest: pass\nlint: pass"],
    )
    .await;
    let run = kit.run_leader(TASK).await;
    let out = run.output.as_ref().expect("the team run succeeded");
    assert!(out.success, "{out:?}");
    assert_eq!(out.response.lines().next(), Some("DONE"));
    assert!(out.response.contains("pull/7") && out.response.contains("CHECKS: PASS"));

    assert_eq!(
        names(&kit),
        [
            "coder-0",
            "tester-0",
            "code-reviewer-0",
            "github-0",
            "github-1",
            "coding-lead-0"
        ]
    );
    let sent = github_prompts(&kit);
    assert!(sent[0].contains(r#""tool":"open_or_update_pr""#));
    assert!(sent[0].contains(r#""draft":true"#) && sent[0].contains("Closes AGE-1"));
    assert!(!sent[0].contains("ship:auto"), "no opt-in, no label");
    assert!(sent[1].contains(r#""tool":"pr_checks""#) && sent[1].contains("1200"));
    // The lead saw the PR link and the verdict before it said DONE.
    let lead_requests = kit.sse.requests_for(LEAD_MODEL);
    assert!(body(&lead_requests[5]).contains("CHECKS: PASS"));
}

/// Red then green: the first CI run fails with `CLASS: CODE`; the failing
/// line reaches the coder word for word, the coder fixes and commits, the
/// tester and reviewer run again, the PR is updated with `ci-fix`, and CI
/// passes.
#[tokio::test]
async fn ci_loop_red_then_green_after_a_coder_fix() {
    let failing = "error: unquoted expansion in calc.sh line 1 (SC2086)";
    let mut coder = coder_adds_sub();
    coder.extend([
        Reply::tool_call(
            "apply_diff",
            serde_json::json!({
                "path": "calc.sh",
                "old_content": "add() { echo $(( $1 + $2 )); }",
                "new_content": "# shellcheck shell=sh\nadd() { echo $(( $1 + $2 )); }",
            }),
        ),
        Reply::tool_call("git_add", serde_json::json!({ "paths": ["calc.sh"] })),
        Reply::tool_call(
            "git_commit",
            serde_json::json!({ "message": "Fix lint: shellcheck directive" }),
        ),
        Reply::text("1. Added the directive. Committed \"Fix lint\"."),
    ]);
    let kit = ci_kit(
        {
            let mut lead = lead_to_approval();
            lead.extend([
                open_pr(&[]),
                pr_checks(),
                check_log("lint"),
                invoke(
                    "coder",
                    &format!("CI check lint failed (CLASS: CODE):\n{failing}"),
                ),
                merge("sub-agent/coder-1"),
                invoke("tester", "Run the repository's checks."),
                invoke("code-reviewer", REVIEW_PROMPT),
                open_pr(&["ci-fix"]),
                pr_checks(),
                Reply::text(format!("DONE\n{PR_UPDATED}\nCHECKS: PASS")),
            ]);
            lead
        },
        coder,
        2,
        &[
            PR_OPENED,
            "CHECKS: FAIL\ntest: pass\nlint: fail",
            &format!("CLASS: CODE\n{failing}"),
            PR_UPDATED,
            "CHECKS: PASS\ntest: pass\nlint: pass",
        ],
    )
    .await;
    let run = kit.run_leader(TASK).await;
    let out = run.output.as_ref().expect("the team run succeeded");
    assert!(out.success, "{out:?}");
    assert_eq!(out.response.lines().next(), Some("DONE"));
    assert!(out.response.contains("CHECKS: PASS"));

    let sent = github_prompts(&kit);
    assert_eq!(sent.len(), 5);
    assert!(sent[2].contains(r#""tool":"failing_check_log""#) && sent[2].contains(r#""lint""#));
    assert!(sent[3].contains(r#""labels":["ci-fix"]"#), "{}", sent[3]);
    // The coder got the failing line word for word.
    let coder_requests = kit.ndjson.requests_for(CODER_MODEL);
    assert!(body(coder_requests.last().unwrap()).contains(failing));
    assert_eq!(
        names(&kit),
        [
            "coder-0",
            "tester-0",
            "code-reviewer-0",
            "github-0",
            "github-1",
            "github-2",
            "coder-1",
            "tester-1",
            "code-reviewer-1",
            "github-3",
            "github-4",
            "coding-lead-0"
        ]
    );
    let lead_branch = "sub-agent/coding-lead-0";
    assert_eq!(
        git(
            &kit.workspace(),
            &[
                "log",
                "--format=%s",
                &format!("{WORKING_BRANCH}..{lead_branch}")
            ]
        ),
        "Fix lint: shellcheck directive\nAdd sub() to calc.sh"
    );
}

/// An infrastructure failure: no code changes; the coder pushes an empty
/// commit once, the lead updates the PR and the rerun passes.
#[tokio::test]
async fn ci_loop_reruns_an_infra_failure_once() {
    let mut coder = coder_adds_sub();
    coder.extend([
        Reply::tool_call(
            "shell_execute",
            serde_json::json!({ "command": "git commit -q --allow-empty -m 'ci: rerun'" }),
        ),
        Reply::text("Committed \"ci: rerun\" (empty)."),
    ]);
    let kit = ci_kit(
        {
            let mut lead = lead_to_approval();
            lead.extend([
                open_pr(&[]),
                pr_checks(),
                check_log("test"),
                invoke(
                    "coder",
                    "Push an empty commit: git commit --allow-empty -m \"ci: rerun\"",
                ),
                merge("sub-agent/coder-1"),
                open_pr(&["ci-fix"]),
                pr_checks(),
                Reply::text(format!("DONE\n{PR_UPDATED}\nCHECKS: PASS")),
            ]);
            lead
        },
        coder,
        1,
        &[
            PR_OPENED,
            "CHECKS: FAIL\ntest: fail",
            "CLASS: INFRA (runner lost communication)\nThe runner received a shutdown signal.",
            PR_UPDATED,
            "CHECKS: PASS\ntest: pass",
        ],
    )
    .await;
    let run = kit.run_leader(TASK).await;
    let out = run.output.as_ref().expect("the team run succeeded");
    assert!(out.success, "{out:?}");
    assert_eq!(out.response.lines().next(), Some("DONE"));

    // No second test or review: the code did not change.
    let n = names(&kit);
    assert_eq!(n.iter().filter(|n| n.starts_with("tester")).count(), 1);
    assert_eq!(
        n.iter().filter(|n| n.starts_with("code-reviewer")).count(),
        1
    );
    assert_eq!(n.iter().filter(|n| n.starts_with("github")).count(), 5);
    assert_eq!(
        git(
            &kit.workspace(),
            &[
                "log",
                "--format=%s",
                &format!("{WORKING_BRANCH}..sub-agent/coding-lead-0")
            ]
        ),
        "ci: rerun\nAdd sub() to calc.sh"
    );
    assert!(body(&kit.sse.requests_for(LEAD_MODEL)[6]).contains("CLASS: INFRA"));
}

// AGE-1048: the lead's state is on the branch. The lead edits nothing, so the
// coder writes `.chatty/plan.md` and `.chatty/progress.md` in the work's
// commits and removes them in a final commit before the PR.

const PLAN_FRESH: &str =
    "# Plan\n\nAcceptance: tests/test_calc.sh passes.\n\n1. [ ] Add sub() to calc.sh\n";
const PLAN_STEP1_DONE: &str = "# Plan\n\nAcceptance: tests/test_calc.sh passes.\n\n\
                               1. [x] Add sub() to calc.sh (abc1234)\n2. [ ] Add mul() to calc.sh\n";
const CALC_WITH_MUL: &str = "add() { echo $(( $1 + $2 )); }\nsub() { echo $(( $1 - $2 )); }\n\
                             mul() { echo $(( $1 * $2 )); }\n";
const REMOVE_STATE: &str = "git rm -q -f .chatty/plan.md .chatty/progress.md && \
                            git commit -q -m \"Remove the lead's state files\"";

fn write_file(path: &str, content: &str) -> Reply {
    Reply::tool_call(
        "write_file",
        serde_json::json!({ "path": path, "content": content }),
    )
}

fn commit(paths: &[&str], message: &str) -> [Reply; 2] {
    [
        Reply::tool_call("git_add", serde_json::json!({ "paths": paths })),
        Reply::tool_call("git_commit", serde_json::json!({ "message": message })),
    ]
}

/// The lead's closing run: tester, reviewer, then the coder removes the
/// state files, the lead merges and replies.
fn lead_finishes(coder_branch: &str, removal_branch: &str) -> Vec<Reply> {
    vec![
        merge(coder_branch),
        invoke("tester", "Run the repository's checks."),
        invoke("code-reviewer", REVIEW_PROMPT),
        invoke(
            "coder",
            &format!("Remove the lead's state files: {REMOVE_STATE}"),
        ),
        merge(removal_branch),
        Reply::text("DONE\nTESTS: PASS; APPROVE."),
    ]
}

fn coder_removes_state() -> Vec<Reply> {
    vec![
        Reply::tool_call(
            "shell_execute",
            serde_json::json!({ "command": REMOVE_STATE }),
        ),
        Reply::text("Committed \"Remove the lead's state files\"."),
    ]
}

/// A kit like `ci_kit` without `github`, for a turn of `testers` test runs.
async fn state_kit(lead: Vec<Reply>, coder: Vec<Reply>) -> SwarmKit {
    let mut tester = Vec::new();
    let mut reviewer = Vec::new();
    tester.extend(tester_passes());
    reviewer.extend(reviewer_approves());
    SwarmKit::start_in_repo(
        vec![
            AgentDef::from_spec(preset("coding-lead"), LEAD_MODEL, Endpoint::Sse),
            AgentDef::from_spec(preset("coder"), CODER_MODEL, Endpoint::Ndjson),
            AgentDef::from_spec(preset("tester"), TESTER_MODEL, Endpoint::Ndjson),
            AgentDef::from_spec(preset("code-reviewer"), REVIEWER_MODEL, Endpoint::Ndjson),
        ],
        Script::new().route(LEAD_MODEL, lead),
        Script::new()
            .route(CODER_MODEL, coder)
            .route(TESTER_MODEL, tester)
            .route(REVIEWER_MODEL, reviewer),
    )
    .await
}

/// A fresh branch: the lead finds no plan, so the first coder call writes
/// `.chatty/plan.md` and commits it before any code; the work's commit marks
/// the step done; a final commit removes the state files, and the diff
/// against the base holds only `calc.sh`.
#[tokio::test]
async fn lead_state_is_written_first_and_removed_before_the_pr() {
    let kit = state_kit(
        {
            let mut lead = vec![
                Reply::tool_call(
                    "read_file",
                    serde_json::json!({ "path": ".chatty/plan.md" }),
                ),
                invoke(
                    "coder",
                    &format!(
                        "There is no .chatty/plan.md: write it first and commit it, with an \
                         empty .chatty/progress.md, before any code. Plan: {PLAN_FRESH}\
                         Then do step 1: {TASK}"
                    ),
                ),
            ];
            lead.extend(lead_finishes("sub-agent/coder-0", "sub-agent/coder-1"));
            lead
        },
        {
            let mut coder = vec![
                write_file(".chatty/plan.md", PLAN_FRESH),
                write_file(".chatty/progress.md", ""),
            ];
            coder.extend(commit(
                &[".chatty/plan.md", ".chatty/progress.md"],
                "Write the plan",
            ));
            coder.extend([
                write_file("calc.sh", CALC_WITH_SUB),
                write_file(
                    ".chatty/plan.md",
                    "# Plan\n\n1. [x] Add sub() to calc.sh (this commit)\n",
                ),
                write_file(
                    ".chatty/progress.md",
                    "2026-11-07: added sub(); nothing failed.\n",
                ),
            ]);
            coder.extend(commit(
                &["calc.sh", ".chatty/plan.md", ".chatty/progress.md"],
                "Add sub() to calc.sh",
            ));
            coder.push(Reply::text("Committed \"Add sub() to calc.sh\"."));
            coder.extend(coder_removes_state());
            coder
        },
    )
    .await;
    let workspace = kit.workspace();
    seed_repository(&workspace, &kit.root().join("remote.git"));
    let base = git(&workspace, &["rev-parse", "main"]);
    let run = kit.run_leader(TASK).await;
    let out = run.output.as_ref().expect("the team run succeeded");
    assert!(out.success, "{out:?}");
    assert_eq!(out.response.lines().next(), Some("DONE"));

    // The very first commit on the branch is the plan, ahead of any code.
    let lead_branch = "sub-agent/coding-lead-0";
    let range = format!("{WORKING_BRANCH}..{lead_branch}");
    assert_eq!(
        git(&workspace, &["log", "--reverse", "--format=%s", &range]),
        "Write the plan\nAdd sub() to calc.sh\nRemove the lead's state files"
    );
    let first = git(&workspace, &["rev-list", "--reverse", &range])
        .lines()
        .next()
        .unwrap()
        .to_string();
    let first_files = git(&workspace, &["show", "--name-only", "--format=", &first]);
    assert!(first_files.contains(".chatty/plan.md"), "{first_files}");
    assert!(!first_files.contains("calc.sh"), "{first_files}");
    // The plan as committed with the work has the step done.
    let mid = git(
        &workspace,
        &["show", &format!("{lead_branch}~1:.chatty/plan.md")],
    );
    assert!(mid.contains("1. [x]"), "{mid}");

    // Before the PR: no `.chatty/` state in the tip, and the diff against the
    // base is the change alone (the repository's own `.chatty/checks` stays).
    let stat = git(
        &workspace,
        &[
            "diff",
            "--stat",
            &format!("{WORKING_BRANCH}..{lead_branch}"),
        ],
    );
    assert!(
        stat.contains("calc.sh") && !stat.contains(".chatty"),
        "{stat}"
    );
    let tree = git(&workspace, &["ls-tree", "-r", "--name-only", lead_branch]);
    assert!(tree.contains(".chatty/checks"), "{tree}");
    assert!(
        !tree.contains("plan.md") && !tree.contains("progress.md"),
        "{tree}"
    );
    assert_eq!(git(&workspace, &["rev-parse", "main"]), base);
}

/// A second turn on a branch whose plan has step 1 done: the lead reads it
/// (the real file reaches its model), and its delegations go straight to
/// step 2. The finished step is not delegated again.
#[tokio::test]
async fn lead_continues_from_the_first_step_not_done() {
    let mut lead = vec![
        Reply::tool_call(
            "read_file",
            serde_json::json!({ "path": ".chatty/plan.md" }),
        ),
        invoke(
            "coder",
            "Step 2 of .chatty/plan.md: add mul(a, b) to calc.sh returning a * b. \
             Mark step 2 [x] and append to .chatty/progress.md in the same commit.",
        ),
    ];
    lead.extend(lead_finishes("sub-agent/coder-0", "sub-agent/coder-1"));
    let mut coder = vec![
        write_file("calc.sh", CALC_WITH_MUL),
        write_file(
            ".chatty/plan.md",
            &PLAN_STEP1_DONE.replace("2. [ ]", "2. [x]"),
        ),
        write_file(".chatty/progress.md", "2026-11-07: step 2 added mul().\n"),
    ];
    coder.extend(commit(
        &["calc.sh", ".chatty/plan.md", ".chatty/progress.md"],
        "Add mul() to calc.sh",
    ));
    coder.push(Reply::text("Committed \"Add mul() to calc.sh\"."));
    coder.extend(coder_removes_state());
    let kit = state_kit(lead, coder).await;
    // The branch a killed lease left behind: sub() done and committed, the
    // plan saying so, the progress file holding the attempt.
    let workspace = kit.workspace();
    std::fs::write(workspace.join("calc.sh"), CALC).unwrap();
    std::fs::create_dir_all(workspace.join("tests")).unwrap();
    std::fs::write(workspace.join("tests/test_calc.sh"), TEST_SCRIPT).unwrap();
    std::fs::create_dir_all(workspace.join(".chatty")).unwrap();
    std::fs::write(workspace.join(".chatty/checks"), "sh tests/test_calc.sh\n").unwrap();
    git(&workspace, &["add", "."]);
    git(
        &workspace,
        &["commit", "-q", "-m", "calc with add, and its test"],
    );
    git(&workspace, &["switch", "-q", "-c", WORKING_BRANCH]);
    std::fs::write(workspace.join("calc.sh"), CALC_WITH_SUB).unwrap();
    std::fs::write(workspace.join(".chatty/plan.md"), PLAN_STEP1_DONE).unwrap();
    std::fs::write(
        workspace.join(".chatty/progress.md"),
        "2026-11-07: step 1 added sub(); no problems.\n",
    )
    .unwrap();
    git(&workspace, &["add", "."]);
    git(&workspace, &["commit", "-q", "-m", "Add sub() to calc.sh"]);
    let base = git(&workspace, &["rev-parse", "main"]);

    let run = kit.run_leader(TASK).await;
    let out = run.output.as_ref().expect("the team run succeeded");
    assert!(out.success, "{out:?}");
    assert_eq!(out.response.lines().next(), Some("DONE"));

    // The plan with step 1 [x] reached the lead before it delegated.
    let lead_requests = kit.sse.requests_for(LEAD_MODEL);
    assert!(
        body(&lead_requests[1]).contains("1. [x] Add sub() to calc.sh (abc1234)"),
        "{}",
        body(&lead_requests[1])
    );
    // One coder call for the work, none for step 1: the delegation is step 2.
    let coder_requests = kit.ndjson.requests_for(CODER_MODEL);
    let first_prompt = body(&coder_requests[0]);
    assert!(first_prompt.contains("Step 2") && !first_prompt.contains("add sub("));
    assert_eq!(
        names(&kit),
        [
            "coder-0",
            "tester-0",
            "code-reviewer-0",
            "coder-1",
            "coding-lead-0"
        ]
    );
    // The new commits are step 2 and the removal; no sub() is written again.
    let lead_branch = "sub-agent/coding-lead-0";
    assert_eq!(
        git(
            &workspace,
            &[
                "log",
                "--format=%s",
                &format!("{WORKING_BRANCH}..{lead_branch}")
            ]
        ),
        "Remove the lead's state files\nAdd mul() to calc.sh"
    );
    assert_eq!(git(&workspace, &["rev-parse", "main"]), base);
}
