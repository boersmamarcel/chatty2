//! The swarm-vs-single benchmark's harness (EV-3, AGE-670), end to end on
//! the fake model.
//!
//! `scripts/swarm-bench/run.sh` drives real `chatty-tui --headless` runs (the
//! binary this crate builds): arm `single` as one agent, arm `swarm` as the
//! family's frozen team (`scripts/swarm-bench/frozen/`), whose leader really spawns its workers. A
//! `FakeDaemon` scripts every agent's turns; the task verifiers run for real;
//! `report.py` turns the results into the report. Needs `bash`, `git` and
//! `python3`.
#![cfg(unix)]

use chatty_core::testing::fake_model::{FakeDaemon, Reply, Script};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

/// SHA-256 of `docs/research/swarm-vs-single-prereg.md`. The
/// pre-registration is frozen before the first real run (AGE-670): a change
/// after that makes the run no longer pre-registered, so it must be
/// deliberate, with this pin updated in the same PR and the reason in its
/// body.
const PREREG_SHA256: &str = "bd083e53672c0da38c3f64b49141678d3ebd883ac45f871be1b0cbbf61137efe";

const MODEL: &str = "fake/bench";
/// The fake runner gives each arm its own model name, so the script can
/// route the single agent's calls (`bench.py`, `write_home`).
const SINGLE: &str = "fake/bench-single";
const TASKS: [&str; 3] = ["d01-region-drop", "c01-pagination", "r01-vendor-initech"];

/// The usage every scripted answer reports: (input, output, cache read).
const USAGE: (u64, u64, u64) = (1000, 100, 0);

/// Each agent's routing key: a phrase of its frozen preamble.
const DATA_LEAD: &str = "You lead a data analysis and compute nothing yourself.";
const DATA_ANALYST: &str = "You answer questions about data files with numbers you computed";
const FIX_LEAD: &str = "You lead a code fix and edit nothing yourself.";
const FIX_CODER: &str = "You fix code in your own git worktree.";
const EDITOR: &str = "You run a research brief and write nothing yourself.";
const RESEARCHER: &str = "You find facts; you do not write prose.";
const WRITER: &str = "You write plain, short briefs from the facts you are given";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root resolves")
}

fn task_dir(name: &str) -> PathBuf {
    repo_root().join("scripts/swarm-bench/tasks").join(name)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn prereg_exists_and_is_frozen() {
    let bytes = std::fs::read(repo_root().join("docs/research/swarm-vs-single-prereg.md"))
        .expect("the pre-registration exists");
    let hash: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        hash, PREREG_SHA256,
        "docs/research/swarm-vs-single-prereg.md changed. It is frozen: a run \
         under a changed protocol is not pre-registered. Change it only on \
         purpose, and update this pin in the same PR."
    );
}

/// One scripted answer: the usage it reports, then the reply.
fn answer(replies: &mut Vec<Reply>, reply: Reply) {
    let (input, output, cache_read) = USAGE;
    replies.push(Reply::Usage {
        input,
        output,
        cache_read,
    });
    replies.push(reply);
}

fn answers(replies: impl IntoIterator<Item = Reply>) -> Vec<Reply> {
    let mut out = Vec::new();
    for reply in replies {
        answer(&mut out, reply);
    }
    out
}

fn write_file(path: &str, content: String) -> Reply {
    Reply::tool_call("write_file", json!({ "path": path, "content": content }))
}

fn invoke(agent: &str, prompt: &str) -> Reply {
    Reply::tool_call("invoke_agent", json!({ "agent": agent, "prompt": prompt }))
}

fn list_agents() -> Reply {
    Reply::tool_call("list_agents", json!({}))
}

/// The scripted run: both arms solve the code and the research task; on the
/// data task the single agent names the right region and the team the
/// wrong one. Returns the script and the number of model calls per arm.
fn script() -> (Script, usize, usize) {
    let fixed_pager = read(&task_dir("c01-pagination").join("solution/pager.py"));
    let brief = read(&task_dir("r01-vendor-initech").join("solution/brief.md"));

    // The single agent, in the runner's task order (d01, c01, r01).
    let single = answers([
        Reply::text("APAC accounts for most of the drop.\nVERDICT: APAC"),
        write_file("pager.py", fixed_pager.clone()),
        Reply::text("Fixed the page start and the page count."),
        write_file("brief.md", brief.clone()),
        Reply::text("Wrote brief.md."),
    ]);
    let single_calls = 5;

    let data_lead = answers([
        list_agents(),
        invoke(
            "data-analyst",
            "Revenue change Aug to Sep by region, in sales.csv",
        ),
        Reply::text("EU drove the drop.\nVERDICT: EU"),
    ]);
    let data_analyst = answers([Reply::text("APAC -24,687; EU -4,917; LATAM -4,665.")]);
    let fix_lead = answers([
        list_agents(),
        invoke("fix-coder", "Make the tests pass without changing them."),
        Reply::tool_call(
            "git_merge",
            json!({ "branch": "sub-agent/fix-coder-0", "no_ff": true }),
        ),
        Reply::text("APPROVE, merged sub-agent/fix-coder-0, tests exit 0."),
    ]);
    let fix_coder = answers([
        write_file("pager.py", fixed_pager),
        Reply::text("Cause: the page start was off by one page. Changed pager.py."),
    ]);
    let editor = answers([
        list_agents(),
        invoke(
            "researcher",
            "Initech Ltd: contract term, cap, DPA, open findings.",
        ),
        invoke("writer", "Write brief.md from these facts."),
        Reply::text("brief.md: Initech Ltd is an active vendor."),
    ]);
    let researcher = answers([Reply::text(
        "1. MSA-2025-11 runs to 2027-03-31 [contracts-index.md]",
    )]);
    let writer = answers([
        write_file("brief.md", brief),
        Reply::text("brief.md, written."),
    ]);
    let swarm_calls = (data_lead.len()
        + data_analyst.len()
        + fix_lead.len()
        + fix_coder.len()
        + editor.len()
        + researcher.len()
        + writer.len())
        / 2;

    // Leaders and workers route by their preambles; the single agent by its
    // model name, last, so it never takes a team agent's call.
    let script = Script::new()
        .route(DATA_LEAD, data_lead)
        .route(DATA_ANALYST, data_analyst)
        .route(FIX_LEAD, fix_lead)
        .route(FIX_CODER, fix_coder)
        .route(EDITOR, editor)
        .route(RESEARCHER, researcher)
        .route(WRITER, writer)
        .route(SINGLE, single);
    (script, single_calls, swarm_calls)
}

fn run(cmd: &mut Command) -> String {
    let out = cmd.output().expect("the command starts");
    assert!(
        out.status.success(),
        "{cmd:?} failed\n--- stdout\n{}\n--- stderr\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn result_json(run_dir: &Path, task: &str, arm: &str) -> Value {
    let path = run_dir
        .join("runs")
        .join(task)
        .join(arm)
        .join("result.json");
    serde_json::from_str(&read(&path)).expect("result.json is JSON")
}

#[test]
fn swarm_bench_dry_run() {
    let root = repo_root();
    let out = tempfile::tempdir().expect("a temp dir");
    let (script, single_calls, swarm_calls) = script();
    let fake = FakeDaemon::scripted(script);

    let log = run(Command::new("bash")
        .arg(root.join("scripts/swarm-bench/run.sh"))
        .args(["--provider", "fake", "--base-url", &fake.base_url()])
        .args(["--model", MODEL, "--arm", "both", "--run-id", "dry"])
        .args(["--only", &TASKS.join(",")])
        .args(["--max-duration", "5m", "--run-timeout", "300"])
        .arg("--out")
        .arg(out.path())
        .arg("--chatty-tui")
        .arg(env!("CARGO_BIN_EXE_chatty-tui")));
    let run_dir = out.path().join("dry");

    // Each run's verdict is the verifier's, on what the run really left.
    let expect = [
        ("d01-region-drop", "single", true),
        ("d01-region-drop", "swarm", false),
        ("c01-pagination", "single", true),
        ("c01-pagination", "swarm", true),
        ("r01-vendor-initech", "single", true),
        ("r01-vendor-initech", "swarm", true),
    ];
    for (task, arm, pass) in expect {
        let r = result_json(&run_dir, task, arm);
        assert_eq!(r["pass"], pass, "{task}/{arm}: {r:#}\n{log}");
        assert_eq!(r["exit_code"], 0, "{task}/{arm}: {r:#}");
    }
    // The team really ran: fix-and-verify opts into worktrees, so its
    // workers' branches exist; research-brief does not (AGE-822), so its
    // writer writes straight into the shared workspace, which is what was
    // judged.
    let fix = result_json(&run_dir, "c01-pagination", "swarm");
    assert_eq!(fix["worker_branches"], json!(["sub-agent/fix-coder-0"]));
    let research = result_json(&run_dir, "r01-vendor-initech", "swarm");
    assert_eq!(research["check"]["found_in"], "workspace", "{research:#}");
    let data = result_json(&run_dir, "d01-region-drop", "swarm");
    assert_eq!(data["check"]["verdict"], "EU");

    // The meter saw every model call of every agent, with its tokens.
    let per_call = USAGE.0 + USAGE.1;
    let mut calls = [0u64, 0u64];
    let mut toks = [0u64, 0u64];
    for task in TASKS {
        for (i, arm) in ["single", "swarm"].iter().enumerate() {
            let m = &result_json(&run_dir, task, arm)["meter"];
            assert_eq!(m["calls_without_usage"], 0, "{m:#}");
            calls[i] += m["calls"].as_u64().unwrap();
            toks[i] += m["input_tokens"].as_u64().unwrap() + m["output_tokens"].as_u64().unwrap();
        }
    }
    assert_eq!(calls, [single_calls as u64, swarm_calls as u64]);
    assert_eq!(
        toks,
        [
            single_calls as u64 * per_call,
            swarm_calls as u64 * per_call
        ]
    );

    // The report: every section, every task, the numbers it was given.
    let report = out.path().join("report.md");
    let numbers = out.path().join("report.json");
    run(Command::new("python3")
        .arg(root.join("scripts/swarm-bench/report.py"))
        .arg(&run_dir)
        .args(["--date", "2026-01-01", "--out"])
        .arg(&report)
        .arg("--json")
        .arg(&numbers));
    let markdown = read(&report);
    let json: Value = serde_json::from_str(&read(&numbers)).expect("the numbers are JSON");
    assert_eq!(json["n_pairs"], 3, "{json:#}");
    assert_eq!(json["stats"]["single"]["solved"], 3);
    assert_eq!(json["stats"]["swarm"]["solved"], 2);
    assert_eq!(json["primary"]["single_only"], 1);
    assert_eq!(json["primary"]["swarm_only"], 0);
    assert_eq!(json["verdict"], "NO_DETECTABLE_DIFFERENCE");
    let ratio = json["secondary"]["tokens_per_solved_ratio"]
        .as_f64()
        .unwrap();
    let expected = (swarm_calls as f64 / 2.0) / (single_calls as f64 / 3.0);
    assert!((ratio - expected).abs() < 1e-9, "{ratio} vs {expected}");
    for section in [
        "# Swarm vs single agent: paired benchmark (2026-01-01)",
        "## Setup",
        "## Primary: success rate",
        "## Secondary: cost and time",
        "## By family (descriptive, not powered)",
        "## Run health",
        "## Per task",
        "**Verdict: NO_DETECTABLE_DIFFERENCE.**",
    ] {
        assert!(
            markdown.contains(section),
            "missing {section:?}\n{markdown}"
        );
    }
    for task in TASKS {
        assert!(markdown.contains(task), "the per-task table lists {task}");
    }
}
