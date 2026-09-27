//! The resume spike's harness (RC-1, AGE-650), end to end on the fake model.
//!
//! `scripts/resume-spike/run.sh` drives real `chatty-tui --headless` workers
//! (the binary this crate builds) against a `FakeDaemon` whose script writes
//! each task's reference solution, then `report.py` turns the results into
//! the report. Two scripted cases: one whose numbers pass the kill
//! criterion, one whose numbers fail it. Needs `bash`, `git` and `python3`.
#![cfg(unix)]

use chatty_core::testing::fake_model::{FakeDaemon, Reply, Script};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

/// SHA-256 of `docs/research/resume-spike-template.md`. Changing the
/// template changes what arm R is told, so results before and after would
/// not pool: a change must be deliberate, with this pin updated in the same
/// PR and the reason in its body.
const TEMPLATE_SHA256: &str = "cb1c1746e66dad71255bbb1f105ab773e3c1099b35cdbc4b82ad0383f5335e57";

/// The fake provider's prices in `spike.py` (`FAKE_PRICES`), USD per million.
const PRICE_INPUT: f64 = 1.0;
const PRICE_OUTPUT: f64 = 4.0;
const PRICE_CACHE_READ: f64 = 0.1;

const MODEL: &str = "fake/spike";
const PAIRS: usize = 3;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root resolves")
}

#[test]
fn rebrief_template_is_frozen() {
    let bytes = std::fs::read(repo_root().join("docs/research/resume-spike-template.md"))
        .expect("the template exists");
    let hash: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        hash, TEMPLATE_SHA256,
        "docs/research/resume-spike-template.md changed. It is frozen: results \
         under another template do not pool. Change it only on purpose, and \
         update this pin in the same PR."
    );
}

/// One model call's usage: `(uncached input, output, cache read)`.
type Usage = (u64, u64, u64);

/// What one follow-up run of an arm does on the fake model.
#[derive(Clone, Copy)]
struct ArmScript {
    /// Writes the task's reference solution (and so passes its verifier).
    solves: bool,
    /// The usage each of its model calls reports.
    usage: Usage,
    /// Milliseconds the fake model waits before each answer.
    delay_ms: u64,
}

impl ArmScript {
    /// Its model calls' usages: one tool call writing the solution then a
    /// closing text, or a single text when it gives up.
    fn calls(&self) -> Vec<Usage> {
        if self.solves {
            vec![self.usage, self.usage]
        } else {
            vec![self.usage]
        }
    }

    fn replies(&self, solution: &[(String, String)]) -> Vec<Reply> {
        let mut replies = Vec::new();
        let answer = |replies: &mut Vec<Reply>, reply: Reply| {
            if self.delay_ms > 0 {
                replies.push(Reply::Delay(self.delay_ms));
            }
            let (input, output, cache_read) = self.usage;
            replies.push(Reply::Usage {
                input,
                output,
                cache_read,
            });
            replies.push(reply);
        };
        if self.solves {
            let writes = solution
                .iter()
                .map(|(path, content)| {
                    (
                        "write_file".to_string(),
                        json!({ "path": path, "content": content }),
                    )
                })
                .collect();
            answer(&mut replies, Reply::ToolCalls(writes));
            answer(&mut replies, Reply::text("Done: the follow-up is in."));
        } else {
            answer(
                &mut replies,
                Reply::text("I could not finish the follow-up."),
            );
        }
        replies
    }
}

/// A task's reference solution, as `(path, content)` pairs relative to the
/// task's repository.
fn solution(task: &Path) -> Vec<(String, String)> {
    let dir = task.join("solution");
    let mut files = Vec::new();
    let mut stack = vec![dir.clone()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("the solution dir") {
            let path = entry.expect("a dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path
                    .strip_prefix(&dir)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                files.push((
                    rel,
                    std::fs::read_to_string(&path).expect("a solution file"),
                ));
            }
        }
    }
    files.sort();
    files
}

/// The first `PAIRS` tasks, in the runner's order.
fn tasks() -> Vec<PathBuf> {
    let mut tasks: Vec<PathBuf> = std::fs::read_dir(repo_root().join("scripts/resume-spike/tasks"))
        .expect("the task set")
        .map(|e| e.expect("a dir entry").path())
        .filter(|p| p.join("task.json").is_file())
        .collect();
    tasks.sort();
    assert!(tasks.len() >= 24, "the spec asks for at least 24 tasks");
    tasks.truncate(PAIRS);
    tasks
}

/// The fake model's script for a whole run: per pair, the first task, then
/// arm C (`resume`), then arm R (`rebrief`), the runner's order.
fn script(tasks: &[PathBuf], arms: &[(ArmScript, ArmScript)]) -> Script {
    let mut replies = Vec::new();
    for (task, (resume, rebrief)) in tasks.iter().zip(arms) {
        let name = task.file_name().unwrap().to_string_lossy();
        replies.push(Reply::Usage {
            input: 1000,
            output: 100,
            cache_read: 0,
        });
        replies.push(Reply::text(format!("First result for {name}: started.")));
        let solution = solution(task);
        replies.extend(resume.replies(&solution));
        replies.extend(rebrief.replies(&solution));
    }
    Script::new().route(MODEL, replies)
}

/// Cost of one arm over the run, as `report.py` computes it: USD at the fake
/// provider's prices, or uncached input + output tokens when unpriced.
fn arm_cost(scripts: &[ArmScript], priced: bool) -> f64 {
    scripts
        .iter()
        .flat_map(ArmScript::calls)
        .map(|(input, output, cache_read)| {
            if priced {
                (input as f64 * PRICE_INPUT
                    + output as f64 * PRICE_OUTPUT
                    + cache_read as f64 * PRICE_CACHE_READ)
                    / 1e6
            } else {
                (input + output) as f64
            }
        })
        .sum()
}

fn run(cmd: &mut Command) {
    let out = cmd.output().expect("the command starts");
    assert!(
        out.status.success(),
        "{cmd:?} failed\n--- stdout\n{}\n--- stderr\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Run the whole pipeline for one scripted case and return the report's
/// markdown and JSON.
fn spike(out: &Path, run_id: &str, provider: &str, fake: &FakeDaemon) -> (String, Value) {
    let root = repo_root();
    run(Command::new("bash")
        .arg(root.join("scripts/resume-spike/run.sh"))
        .args(["--provider", provider, "--base-url", &fake.base_url()])
        .args(["--model", MODEL, "--pairs", &PAIRS.to_string()])
        .args(["--condition", "cold", "--cold-wait", "0"])
        .args(["--max-duration", "5m", "--run-id", run_id])
        .arg("--out")
        .arg(out)
        .arg("--chatty-tui")
        .arg(env!("CARGO_BIN_EXE_chatty-tui")));
    let report = out.join(format!("{run_id}.md"));
    let numbers = out.join(format!("{run_id}.json"));
    run(Command::new("python3")
        .arg(root.join("scripts/resume-spike/report.py"))
        .arg(out.join(run_id))
        .args(["--min-pairs", &PAIRS.to_string(), "--date", "2026-01-01"])
        .arg("--out")
        .arg(&report)
        .arg("--json")
        .arg(&numbers));
    let markdown = std::fs::read_to_string(&report).expect("the report");
    let json: Value =
        serde_json::from_str(&std::fs::read_to_string(&numbers).expect("the numbers"))
            .expect("the numbers are JSON");
    (markdown, json)
}

fn pair_json(out: &Path, run_id: &str, index: usize, task: &Path) -> Value {
    let name = format!(
        "{:02}-{}",
        index + 1,
        task.file_name().unwrap().to_string_lossy()
    );
    let path = out.join(run_id).join("pairs").join(name).join("pair.json");
    serde_json::from_str(&std::fs::read_to_string(&path).expect("pair.json")).unwrap()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * b.abs().max(1.0)
}

#[test]
fn resume_spike_dry_run() {
    let tasks = tasks();
    let out = tempfile::tempdir().expect("a temp dir");

    // ── Case 1: priced, arm C cheaper and faster, both solve every task ──
    // Arm C's calls hit the cache for most of the prompt; arm R pays full
    // price and is slowed down, so the verdict is PASS by a wide margin.
    let resume = ArmScript {
        solves: true,
        usage: (200, 50, 800),
        delay_ms: 0,
    };
    let rebrief = ArmScript {
        solves: true,
        usage: (1000, 50, 0),
        delay_ms: 1500,
    };
    let arms = vec![(resume, rebrief); PAIRS];
    let fake = FakeDaemon::scripted(script(&tasks, &arms));
    let (markdown, numbers) = spike(out.path(), "pass", "fake", &fake);

    let group = &numbers["groups"][0];
    assert_eq!(group["valid_pairs"], PAIRS, "{numbers:#}");
    let r = &group["stats"]["rebrief"];
    let c = &group["stats"]["resume"];
    assert_eq!(
        (r["solved"].as_u64(), c["solved"].as_u64()),
        (Some(3), Some(3))
    );
    let r_cost = arm_cost(&[rebrief; PAIRS], true);
    let c_cost = arm_cost(&[resume; PAIRS], true);
    assert!(
        close(r["cost_per_solved"].as_f64().unwrap(), r_cost / 3.0),
        "{r}"
    );
    assert!(
        close(c["cost_per_solved"].as_f64().unwrap(), c_cost / 3.0),
        "{c}"
    );
    assert!(
        close(
            group["judgement"]["cost_reduction"].as_f64().unwrap(),
            100.0 * (1.0 - c_cost / r_cost)
        ),
        "{group:#}"
    );
    assert_eq!(numbers["verdict"], "PASS", "{markdown}");
    assert!(markdown.contains("**Verdict: PASS**"), "{markdown}");
    for task in &tasks {
        let name = task.file_name().unwrap().to_string_lossy();
        assert!(markdown.contains(&*name), "the paired table lists {name}");
    }

    // Arm C really resumed: its first request carries the first task's
    // conversation ahead of the resume prompt. Arm R got the re-brief.
    let bodies: Vec<String> = fake
        .requests()
        .iter()
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .collect();
    for (index, task) in tasks.iter().enumerate() {
        let spec: Value =
            serde_json::from_str(&std::fs::read_to_string(task.join("task.json")).unwrap())
                .unwrap();
        let task_text = serde_json::to_string(spec["task"].as_str().unwrap()).unwrap();
        let task_text = task_text.trim_matches('"');
        let name = task.file_name().unwrap().to_string_lossy();
        let first_answer = format!("First result for {name}");
        let resumed = bodies
            .iter()
            .find(|b| b.contains("Since your last task ended") && b.contains(task_text))
            .unwrap_or_else(|| panic!("no resumed request for {name}"));
        assert!(
            resumed.find(task_text) < resumed.find("Since your last task ended")
                && resumed.contains(&first_answer),
            "arm C's request replays the first conversation before the follow-up"
        );
        let rebriefed = bodies
            .iter()
            .find(|b| b.contains("## The original task") && b.contains(&first_answer))
            .unwrap_or_else(|| panic!("no re-brief request for {name}"));
        assert!(!rebriefed.contains("Since your last task ended"));

        let pair = pair_json(out.path(), "pass", index, task);
        assert_eq!(pair["valid"], true, "{pair:#}");
        assert!(pair["first"]["conversation_messages"].as_u64().unwrap() >= 2);
        for arm in ["resume", "rebrief"] {
            assert_eq!(pair["arms"][arm]["pass"], true, "{arm}: {pair:#}");
            assert!(pair["arms"][arm]["divergence"]["files"].as_u64().unwrap() >= 1);
        }
    }

    // ── Case 2: unpriced (Ollama-shaped, through the prompt-eval meter),
    // arm C no cheaper and failing one follow-up: the verdict is FAIL ──
    let same = ArmScript {
        solves: true,
        usage: (1000, 50, 0),
        delay_ms: 0,
    };
    let gives_up = ArmScript {
        solves: false,
        ..same
    };
    let arms = vec![(same, same), (gives_up, same), (same, same)];
    let fake = FakeDaemon::scripted(script(&tasks, &arms));
    let (markdown, numbers) = spike(out.path(), "fail", "ollama", &fake);

    let group = &numbers["groups"][0];
    assert_eq!(group["pricing"], Value::Null, "an Ollama run is unpriced");
    let r = &group["stats"]["rebrief"];
    let c = &group["stats"]["resume"];
    assert_eq!(
        (r["solved"].as_u64(), c["solved"].as_u64()),
        (Some(3), Some(2))
    );
    let r_cost = arm_cost(&[same, same, same], false);
    let c_cost = arm_cost(&[same, gives_up, same], false);
    assert!(
        close(r["cost_per_solved"].as_f64().unwrap(), r_cost / 3.0),
        "{r}"
    );
    assert!(
        close(c["cost_per_solved"].as_f64().unwrap(), c_cost / 2.0),
        "{c}"
    );
    assert!(close(
        group["judgement"]["pass_delta"].as_f64().unwrap(),
        -100.0 / 3.0
    ));
    assert_eq!(numbers["verdict"], "FAIL", "{markdown}");
    assert!(markdown.contains("**Verdict: FAIL**"), "{markdown}");
    assert!(markdown.contains("tokens (uncached input + output)"));

    // The meter saw every model call of each arm, with its prompt size.
    let pair = pair_json(out.path(), "fail", 1, &tasks[1]);
    assert_eq!(pair["arms"]["resume"]["pass"], false);
    assert_eq!(
        pair["arms"]["resume"]["prompt_eval"]["calls"], 1,
        "{pair:#}"
    );
    assert_eq!(
        pair["arms"]["resume"]["prompt_eval"]["prompt_eval_count"],
        1000
    );
    assert_eq!(
        pair["arms"]["rebrief"]["prompt_eval"]["calls"], 2,
        "{pair:#}"
    );
}
