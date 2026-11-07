//! The curation team (AGE-1053) on the swarm kit: the `curator-lead`,
//! `scout`, `spec-writer` and `example-checker` presets run as real
//! `chatty-tui` workers in a small git repository, behind a scripted fake
//! model, and turn a source (or a failed item) into swarm-ready items.
//!
//! What is real: every worker process, its preset spec (profile, delegation,
//! budget), its own `git worktree`, and the scout's and example-checker's
//! shell, whose real output (an example's value, a grep for a mutation line,
//! a failed branch's file) reaches their models. What is scripted: each
//! model's replies, so the verdicts are the script's words. The lead's final
//! answer goes through `coding::parse_items`, and every item it carries
//! through `coding::issue_readiness`, as the queue will (hive phase 5b).

use std::path::Path;

use chatty_core::agent_spec::{AgentSpec, load_agent_spec_from};
use chatty_core::coding::{
    Item, ParseItemsError, ParsedItems, PartialWork, Ship, issue_readiness, parse_items,
};
use chatty_core::testing::fake_model::{RecordedRequest, Reply, Script};

use super::swarm_kit::{AgentDef, Endpoint, SwarmKit, git};

const LEAD_MODEL: &str = "kit/curator-lead";
const SCOUT_MODEL: &str = "kit/scout";
const WRITER_MODEL: &str = "kit/spec-writer";
const CHECKER_MODEL: &str = "kit/example-checker";

/// The repository: `add` is tested, `div` (integer division) is not.
const CALC: &str = "add() { echo $(( $1 + $2 )); }\ndiv() { echo $(( $1 / $2 )); }\n";
const TEST_SCRIPT: &str = r#". ./calc.sh
[ "$(add 2 3)" = 5 ] || { echo "FAIL: add 2 3 gave '$(add 2 3)', want 5"; exit 1; }
echo "test result: ok. 1 passed; 0 failed"
"#;

/// A vague source, as a debrief's action item reads.
const SOURCE: &str = "Debrief action item: calc.sh's div has no tests. Add some.";

/// The one item the team should end with: every example holds against
/// `calc.sh`, and the mutation line is there word for word.
const DIV_ITEM: &str = "> **Repo:** `acme/calc` · tests for div

## Why

`div` in calc.sh has no test, and integer division is easy to get wrong.

## Repo and files

* `tests/test_calc.sh` — add two div checks.
* Only these files change.

## Change

1. `div 8 2` gives `4`.
2. `div 7 2` gives `3` (integer division rounds down).

## Check commands

* `sh tests/test_calc.sh`

## Acceptance criteria

* `sh tests/test_calc.sh` passes, with the two new checks.
* Only `tests/test_calc.sh` changes.

## Mutation check

* Both checks: in `calc.sh`, changing `div() { echo $(( $1 / $2 )); }` to `div() { echo $(( $1 * $2 )); }` fails them.

## Size

About 2 lines in 1 file. Builds: nothing (shell only).

## Ship policy

`ship:auto` allowed: test code only, no billing, credentials or security.";

/// The scout's wrong example, as the spec-writer first copied it.
fn div_item_wrong() -> String {
    DIV_ITEM.replace(
        "`div 7 2` gives `3` (integer division rounds down)",
        "`div 7 2` gives `3.5`",
    )
}

/// An item's block in the `items` grammar: `ship: auto` and no other
/// header, so its conflict keys are the paths under Repo and files.
fn block(id: &str, item: &str) -> String {
    block_with(id, "ship: auto", item)
}

fn block_with(id: &str, headers: &str, item: &str) -> String {
    format!("--- ITEM {id} ---\n{headers}\n\n{item}\n--- END ITEM ---")
}

/// The command the example-checker runs for the item's examples.
const RUN_EXAMPLES: &str =
    ". ./calc.sh; echo \"div 8 2 = $(div 8 2)\"; echo \"div 7 2 = $(div 7 2)\"";
/// Its dry mutation check: the named line exists, word for word.
const GREP_MUTATION_LINE: &str = "grep -n -F 'div() { echo $(( $1 / $2 )); }' calc.sh";

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

/// Who the broker's edge log says was called, in the order the calls ended.
fn callees(kit: &SwarmKit) -> Vec<String> {
    let log = std::fs::read_to_string(kit.broker().edge_log_path()).expect("the edge log");
    log.lines()
        .map(|line| {
            let row: serde_json::Value = serde_json::from_str(line).expect("a JSON row");
            assert_eq!(row["outcome"], "completed", "{row}");
            row["to"].as_str().unwrap_or_default().to_string()
        })
        .collect()
}

fn invoke(agent: &str, prompt: &str) -> Reply {
    Reply::tool_call(
        "invoke_agent",
        serde_json::json!({ "agent": agent, "prompt": prompt }),
    )
}

fn shell(command: &str) -> Reply {
    Reply::tool_call("shell_execute", serde_json::json!({ "command": command }))
}

/// The items of the lead's answer, each checked as the queue will: the
/// answer parses, and every item passes the coding readiness check.
fn ready_items(answer: &str) -> ParsedItems {
    let parsed = parse_items(answer).unwrap_or_else(|e| panic!("{e}: {answer}"));
    for item in &parsed.items {
        let text = &item.text;
        assert_eq!(issue_readiness(text), Ok(()), "not swarm-ready:\n{text}");
    }
    parsed
}

/// The repository the source is about, committed on `main`.
fn seed_repository(workspace: &Path) {
    std::fs::write(workspace.join("calc.sh"), CALC).unwrap();
    std::fs::create_dir_all(workspace.join("tests")).unwrap();
    std::fs::write(workspace.join("tests/test_calc.sh"), TEST_SCRIPT).unwrap();
    std::fs::create_dir_all(workspace.join(".chatty")).unwrap();
    std::fs::write(workspace.join(".chatty/checks"), "sh tests/test_calc.sh\n").unwrap();
    git(
        workspace,
        &["add", "calc.sh", "tests/test_calc.sh", ".chatty/checks"],
    );
    git(workspace, &["commit", "-q", "-m", "calc with add and div"]);
}

/// A kit with the four presets, the lead on SSE and the members on NDJSON.
async fn curation_kit(
    lead: Vec<Reply>,
    scout: Vec<Reply>,
    writer: Vec<Reply>,
    checker: Vec<Reply>,
) -> SwarmKit {
    let kit = SwarmKit::start_in_repo(
        vec![
            AgentDef::from_spec(preset("curator-lead"), LEAD_MODEL, Endpoint::Sse),
            AgentDef::from_spec(preset("scout"), SCOUT_MODEL, Endpoint::Ndjson),
            AgentDef::from_spec(preset("spec-writer"), WRITER_MODEL, Endpoint::Ndjson),
            AgentDef::from_spec(preset("example-checker"), CHECKER_MODEL, Endpoint::Ndjson),
        ],
        Script::new().route(LEAD_MODEL, lead),
        Script::new()
            .route(SCOUT_MODEL, scout)
            .route(WRITER_MODEL, writer)
            .route(CHECKER_MODEL, checker),
    )
    .await;
    seed_repository(&kit.workspace());
    kit
}

/// Curation writes nothing: no worker's branch moved off `main`.
fn assert_nothing_committed(kit: &SwarmKit) {
    let workspace = kit.workspace();
    let base = git(&workspace, &["rev-parse", "main"]);
    let branches = git(
        &workspace,
        &[
            "for-each-ref",
            "--format=%(refname:short) %(objectname)",
            "refs/heads/sub-agent",
        ],
    );
    for line in branches.lines() {
        assert!(line.ends_with(&base), "a curation worker committed: {line}");
    }
}

/// The roles' tools, as the fake models were offered them: nobody writes,
/// commits or merges; the lead delegates; scout and example-checker have
/// the shell, spec-writer does not.
fn assert_roles(kit: &SwarmKit) {
    let offered = |requests: Vec<RecordedRequest>| tools(&requests[0]);
    let lead = offered(kit.sse.requests_for(LEAD_MODEL));
    assert!(lead.iter().any(|t| t == "invoke_agent"), "{lead:?}");
    let mut roles = vec![("curator-lead", lead, false)];
    for (name, model, has_shell) in [
        ("scout", SCOUT_MODEL, true),
        ("spec-writer", WRITER_MODEL, false),
        ("example-checker", CHECKER_MODEL, true),
    ] {
        let requests = kit.ndjson.requests_for(model);
        if !requests.is_empty() {
            roles.push((name, offered(requests), has_shell));
        }
    }
    for (name, tools, has_shell) in roles {
        for tool in [
            "write_file",
            "apply_diff",
            "delete_file",
            "git_add",
            "git_commit",
            "git_merge",
            "execute_code",
        ] {
            assert!(
                !tools.iter().any(|t| t == tool),
                "{name}: {tool} in {tools:?}"
            );
        }
        assert_eq!(
            tools.iter().any(|t| t == "shell_execute"),
            has_shell,
            "{name}'s shell: {tools:?}"
        );
    }
}

/// A vague source becomes one swarm-ready item, and the wrong example the
/// scout gave (`div 7 2` is `3.5`, read and not run) is caught: the
/// example-checker really runs it, the shell says `3`, it answers
/// `EXAMPLES: MISMATCH`; the lead spends its one fix round on spec-writer,
/// the checker runs the examples again and answers `EXAMPLES: OK`, and the
/// lead's `ITEMS: 1` carries the corrected item.
#[tokio::test]
async fn curation_team_turns_a_vague_source_into_one_checked_item() {
    let mismatch = "EXAMPLES: MISMATCH\n1. Item div-tests, Change 2: `div 7 2` is expected to \
                    give `3.5`; `. ./calc.sh; div 7 2` printed 3. Fix: expect `3`.";
    let kit = curation_kit(
        vec![
            invoke(
                "scout",
                &format!(
                    "Source: {SOURCE}\nFind the exact files, functions and lines, the test \
                     set-up and the check commands."
                ),
            ),
            invoke(
                "spec-writer",
                "Source: calc.sh's div has no tests.\nScout's findings:\n1. calc.sh:2 \
                 `div() { echo $(( $1 / $2 )); }`. div 7 2 gives 3.5 (read, not run).\n\
                 2. tests/test_calc.sh is the test; .chatty/checks runs it.",
            ),
            invoke(
                "example-checker",
                &format!(
                    "Check every example, expected value and mutation line against the code:\n{}",
                    block("div-tests", &div_item_wrong())
                ),
            ),
            invoke(
                "spec-writer",
                &format!(
                    "Fix round. Your item:\n{}\nexample-checker's mismatches:\n{mismatch}",
                    block("div-tests", &div_item_wrong())
                ),
            ),
            invoke(
                "example-checker",
                &format!(
                    "Check every example, expected value and mutation line against the code:\n{}",
                    block("div-tests", DIV_ITEM)
                ),
            ),
            Reply::text(format!("ITEMS: 1\n{}", block("div-tests", DIV_ITEM))),
        ],
        vec![
            shell("grep -n 'div' calc.sh; cat .chatty/checks; ls tests"),
            Reply::text(
                "1. calc.sh:2 `div() { echo $(( $1 / $2 )); }`. div 7 2 gives 3.5 (read, not \
                 run).\n2. tests/test_calc.sh is the test; .chatty/checks runs it.",
            ),
        ],
        vec![
            Reply::text(block("div-tests", &div_item_wrong())),
            Reply::text(format!(
                "1. Change 2 now expects `3`.\n{}",
                block("div-tests", DIV_ITEM)
            )),
        ],
        vec![
            shell(RUN_EXAMPLES),
            shell(GREP_MUTATION_LINE),
            Reply::text(mismatch),
            shell(RUN_EXAMPLES),
            shell(GREP_MUTATION_LINE),
            Reply::text(
                "EXAMPLES: OK\nChange 1: div 8 2 = 4, RAN\nChange 2: div 7 2 = 3, RAN\n\
                 Mutation line calc.sh:2, RAN",
            ),
        ],
    )
    .await;

    let run = kit.run_leader(SOURCE).await;
    let out = run.output.as_ref().expect("the curation run succeeded");
    assert!(out.success, "{out:?}");

    // The steps in order: scout, spec-writer, example-checker, one fix round.
    assert_eq!(
        callees(&kit),
        [
            "scout-0",
            "spec-writer-0",
            "example-checker-0",
            "spec-writer-1",
            "example-checker-1",
            "curator-lead-0",
        ]
    );
    assert_roles(&kit);

    // The scout's shell really read the repository.
    let scout = kit.ndjson.requests_for(SCOUT_MODEL);
    assert!(body(&scout[1]).contains("2:div() { echo $(( $1 / $2 )); }"));

    // The checker really ran the examples: the shell says div 7 2 is 3,
    // and the mutation line is in calc.sh word for word.
    let checker = kit.ndjson.requests_for(CHECKER_MODEL);
    assert_eq!(checker.len(), 6);
    let ran = body(&checker[1]);
    assert!(
        ran.contains("div 8 2 = 4") && ran.contains("div 7 2 = 3") && !ran.contains("= 3.5"),
        "the examples ran: {ran}"
    );
    assert!(body(&checker[2]).contains("2:div() { echo $(( $1 / $2 )); }"));
    // Its mismatch reached spec-writer word for word, and the fixed item
    // was checked again.
    let writer = kit.ndjson.requests_for(WRITER_MODEL);
    assert!(body(&writer[1]).contains(mismatch));
    assert!(body(&checker[3]).contains("`div 7 2` gives `3` (integer division rounds down)"));
    let lead = kit.sse.requests_for(LEAD_MODEL);
    assert!(body(&lead[3]).contains("EXAMPLES: MISMATCH"));
    assert!(body(&lead[5]).contains("EXAMPLES: OK"));

    // The answer is one swarm-ready item, the corrected one.
    // Its id and headers come through: it ships automatically, depends on
    // nothing, and conflicts on the one path under Repo and files.
    let parsed = ready_items(&out.response);
    assert_eq!(
        parsed.items,
        [Item {
            id: "div-tests".to_string(),
            text: DIV_ITEM.to_string(),
            depends_on: vec![],
            conflict_keys: vec!["tests/test_calc.sh".to_string()],
            ship: Ship::Auto,
        }]
    );
    assert_eq!(parsed.partial_work, None, "a source is no replan");
    assert!(!parsed.items[0].text.contains("3.5"));
    assert_nothing_committed(&kit);
}

/// A source with nothing to change in this repository: the scout looks,
/// really lists the repository, answers `NOTHING FOUND`, and the lead
/// replies `REJECT`, which `parse_items` reads as a rejection with its
/// reason.
#[tokio::test]
async fn curation_team_rejects_a_source_that_is_not_codable() {
    let source = "Debrief action item: publish the release notes for calc 2.0.";
    let kit = curation_kit(
        vec![
            invoke(
                "scout",
                &format!(
                    "Source: {source}\nIs there anything in this repository the source asks \
                     to change?"
                ),
            ),
            Reply::text(
                "REJECT: release notes are published outside this repository; scout found \
                 nothing in it to change (NOTHING FOUND).",
            ),
        ],
        vec![
            shell("git ls-files"),
            Reply::text(
                "NOTHING FOUND\nThe repository holds calc.sh, tests/test_calc.sh and \
                 .chatty/checks: no release notes, changelog or docs.",
            ),
        ],
        vec![],
        vec![],
    )
    .await;

    let run = kit.run_leader(source).await;
    let out = run.output.as_ref().expect("the curation run succeeded");
    assert!(out.success, "{out:?}");
    assert_eq!(callees(&kit), ["scout-0", "curator-lead-0"]);
    assert_roles(&kit);
    let scout = kit.ndjson.requests_for(SCOUT_MODEL);
    assert!(body(&scout[1]).contains("tests/test_calc.sh"));
    assert!(body(&kit.sse.requests_for(LEAD_MODEL)[1]).contains("NOTHING FOUND"));

    match parse_items(&out.response) {
        Err(ParseItemsError::Rejected(why)) => {
            assert!(why.contains("outside this repository"), "{why}")
        }
        other => panic!("not a rejection: {other:?}"),
    }
    assert_nothing_committed(&kit);
}

/// The failed item: tests for `div` and for a `mod` calc.sh does not have.
const FAILED_ITEM: &str = "> **Repo:** `acme/calc` · tests for div and mod

## Repo and files

* `tests/test_calc.sh` — add div and mod checks.
* Only these files change.

## Change

1. `div 7 2` gives `3.5`.
2. `mod 7 2` gives `1`.

## Check commands

* `sh tests/test_calc.sh`

## Acceptance criteria

* `sh tests/test_calc.sh` passes, with the new checks.

## Mutation check

* In `calc.sh`, changing `mod() { echo $(( $1 % $2 )); }` to `+` fails the mod check.

## Size

About 4 lines in 1 file.

## Ship policy

`ship:auto` allowed: test code only.";

/// The failed branch's one commit: the checks as the failed item had them.
const FAILED_TEST_SCRIPT: &str = r#". ./calc.sh
[ "$(div 7 2)" = 3.5 ] || { echo "FAIL: div 7 2 gave '$(div 7 2)', want 3.5"; exit 1; }
[ "$(mod 7 2)" = 1 ] || { echo "FAIL: mod 7 2 gave '$(mod 7 2)', want 1"; exit 1; }
"#;
const FAILED_BRANCH: &str = "auto/calc-1-div-mod";
/// The replacement's headers, given explicitly this time.
const REPLAN_HEADERS: &str = "conflict-keys: tests/test_calc.sh, calc.sh\nship: auto";

/// A replan: the input is the failed item, its failure and its branch's
/// state files. The scout really reads the failed branch; the lead's answer
/// is one narrower item (div only, with the right value), never the failed
/// item again, and it says the partial work is unsound.
#[tokio::test]
async fn curation_team_replans_a_failed_item_into_a_narrower_one() {
    let reason = "NOT DONE: tests/test_calc.sh fails: div 7 2 gave '3', want 3.5; \
                  mod: not found.";
    let input = format!(
        "Replan. Failed item:\n{FAILED_ITEM}\nReason: {reason}\nBranch: {FAILED_BRANCH}\n\
         .chatty/plan.md:\n1. [x] div and mod checks (4d2c1e0)\n.chatty/progress.md:\n\
         2026-11-07 attempt 1: the checks fail; calc.sh has no mod."
    );
    let kit = curation_kit(
        vec![
            invoke(
                "scout",
                &format!(
                    "Replan. Reason: {reason}\nRead {FAILED_BRANCH}: what did it get right? \
                     Find the files, lines, test set-up and check commands for div."
                ),
            ),
            invoke(
                "spec-writer",
                &format!(
                    "Replan; write a narrower item than the failed one, never the failed item \
                     again. Reason: {reason}\nScout's findings:\n1. calc.sh:2 div exists; \
                     there is no mod.\n2. div 7 2 gives 3 (ran)."
                ),
            ),
            invoke(
                "example-checker",
                &format!(
                    "Check every example, expected value and mutation line:\n{}",
                    block_with("div-only", REPLAN_HEADERS, DIV_ITEM)
                ),
            ),
            Reply::text(format!(
                "ITEMS: 1\nPARTIAL WORK: UNSOUND: the branch's one commit expects div 7 2 to \
                 give 3.5 and tests a mod that does not exist.\n{}",
                block_with("div-only", REPLAN_HEADERS, DIV_ITEM)
            )),
        ],
        vec![
            shell(&format!(
                "git log --oneline main..{FAILED_BRANCH}; \
                 git show {FAILED_BRANCH}:tests/test_calc.sh; grep -n 'mod' calc.sh; echo mod-grep $?"
            )),
            Reply::text(
                "1. calc.sh:2 div exists; there is no mod (grep exit 1).\n2. The failed \
                 branch's commit expects div 7 2 = 3.5; the code gives 3 (ran). Not sound.",
            ),
        ],
        vec![Reply::text(block_with("div-only", REPLAN_HEADERS, DIV_ITEM))],
        vec![
            shell(RUN_EXAMPLES),
            shell(GREP_MUTATION_LINE),
            Reply::text("EXAMPLES: OK\nChange 1, RAN\nChange 2, RAN\nMutation line, RAN"),
        ],
    )
    .await;
    // The failed branch, as the attempt left it.
    let workspace = kit.workspace();
    git(&workspace, &["switch", "-q", "-c", FAILED_BRANCH]);
    std::fs::write(workspace.join("tests/test_calc.sh"), FAILED_TEST_SCRIPT).unwrap();
    git(
        &workspace,
        &["commit", "-q", "-am", "Add div and mod checks"],
    );
    git(&workspace, &["switch", "-q", "main"]);

    let run = kit.run_leader(&input).await;
    let out = run.output.as_ref().expect("the curation run succeeded");
    assert!(out.success, "{out:?}");
    assert_eq!(
        callees(&kit),
        [
            "scout-0",
            "spec-writer-0",
            "example-checker-0",
            "curator-lead-0"
        ]
    );
    assert_roles(&kit);

    // The scout really read the failed branch and found no mod.
    let scout = body(&kit.ndjson.requests_for(SCOUT_MODEL)[1]);
    assert!(scout.contains("Add div and mod checks"), "{scout}");
    assert!(
        scout.contains("want 3.5") && scout.contains("mod-grep 1"),
        "{scout}"
    );
    // The failure reason reached the scout and the spec-writer.
    assert!(body(&kit.ndjson.requests_for(SCOUT_MODEL)[0]).contains(reason));
    assert!(body(&kit.ndjson.requests_for(WRITER_MODEL)[0]).contains(reason));

    // One item, swarm-ready, not the failed one, and narrower: div only,
    // fewer steps, the wrong value gone.
    let parsed = ready_items(&out.response);
    assert_eq!(parsed.items.len(), 1);
    let replacement = &parsed.items[0];
    assert_eq!(replacement.id, "div-only");
    assert_eq!(replacement.conflict_keys, ["tests/test_calc.sh", "calc.sh"]);
    assert_eq!(replacement.ship, Ship::Auto);
    assert!(replacement.depends_on.is_empty());
    let item = &replacement.text;
    assert_ne!(item.trim(), FAILED_ITEM.trim());
    assert!(!item.contains("mod") && FAILED_ITEM.contains("mod"));
    assert!(!item.contains("3.5"));
    let steps = |text: &str| {
        text.split("## Change")
            .nth(1)
            .and_then(|rest| rest.split("\n## ").next())
            .map(|change| {
                change
                    .lines()
                    .filter(|l| l.trim_start().starts_with(char::is_numeric))
                    .count()
            })
            .unwrap()
    };
    assert_eq!(steps(FAILED_ITEM), 2);
    assert!(steps(item) <= steps(FAILED_ITEM));
    // It says whether the failed branch's work is sound.
    assert!(
        matches!(&parsed.partial_work, Some(PartialWork::Unsound(why)) if why.contains("3.5")),
        "{:?}",
        parsed.partial_work
    );
    assert_nothing_committed(&kit);
}
