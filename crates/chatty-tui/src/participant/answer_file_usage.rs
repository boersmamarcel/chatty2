//! AGE-854: a worker whose run ends through the answer-file stop still
//! reports what it spent to its caller.
//!
//! In a Harbor container the task asks for `/app/answer.txt`. A worker that
//! writes it is stopped by headless at once (`stops_on_answer_file`), which
//! cancels its turn before the provider's turn aggregate arrives. Its usage
//! then only reaches the caller if the cancelled turn still emits what its
//! calls spent (AGE-864). On the host the task names no answer file, every
//! worker turn ends on its own, and usage was always reported.
//!
//! The kit's workers already run with a fresh `HOME` (its wrapper points
//! `HOME` and the XDG dirs at a temp dir) against an OpenAI-compatible fake,
//! the same OpenRouter-type client `--openai-compat-url` builds.

use serde_json::json;

use super::swarm_kit::{AgentDef, Endpoint, SwarmKit};
use chatty_core::agent_spec::BestOfSection;
use chatty_core::testing::fake_model::{Reply, Script};
use chatty_core::tools::best_of_tool::{BestOfArgs, BestOfTool};
use chatty_core::tools::invoke_agent_tool::InvokeAgentProgress;
use rig_agent::tool::{Tool, ToolContext};

/// Names an answer file, as the DABstep task text does.
const TASK: &str = "How many rows does payments.csv have? Write the answer to answer.txt.";

/// One worker turn: read a file, then write the answer file, each call
/// reporting its own usage. The answer-file stop cancels the turn after the
/// write; the reply after it is only reached if that stop came too late, and
/// its delay keeps it from completing before the cancel lands.
fn ends_on_answer_file(answer: &str) -> Vec<Reply> {
    vec![
        Reply::Usage {
            input: 1_000,
            output: 50,
            cache_read: 0,
        },
        Reply::tool_call("read_file", json!({ "path": "README.md" })),
        Reply::Usage {
            input: 2_000,
            output: 60,
            cache_read: 0,
        },
        Reply::Say(format!("FINAL ANSWER: {answer}")),
        Reply::tool_call(
            "write_file",
            json!({ "path": "answer.txt", "content": format!("{answer}\n") }),
        ),
        Reply::Delay(5_000),
        Reply::text("Too late: the answer-file stop should have ended this turn."),
    ]
}

/// The usage `invoke_agent` hands its caller for a worker that ended on
/// the answer-file stop: both of its calls, named for the worker.
#[tokio::test]
async fn a_worker_ended_by_its_answer_file_reports_its_usage() {
    let kit = SwarmKit::start(
        vec![AgentDef::new("kit-solver", "kit/solver", Endpoint::Sse)],
        Script::new().route("kit/solver", ends_on_answer_file("42")),
        Script::new(),
    )
    .await;

    let run = kit.run_leader(TASK).await;

    let out = run.output.expect("the delegation succeeds");
    assert_eq!(
        std::fs::read_to_string(kit.workspace().join("answer.txt")).expect("the answer file"),
        "42\n"
    );
    assert!(
        !out.response.contains("Too late"),
        "the run ended on the answer-file stop: {}",
        out.response
    );
    let input: u32 = out.usage.iter().map(|line| line.input_tokens).sum();
    let output: u32 = out.usage.iter().map(|line| line.output_tokens).sum();
    assert_eq!(
        (input, output),
        (3_000, 110),
        "both calls of the cancelled turn reach the caller: {:?}",
        out.usage
    );
    assert!(
        out.usage
            .iter()
            .all(|line| line.delegated_to.as_deref() == Some("kit-solver")),
        "{:?}",
        out.usage
    );
    // What the leader's `--usage-file` folds into `delegated_by_agent`.
    let finished = run
        .progress
        .iter()
        .find_map(|event| match event {
            InvokeAgentProgress::Finished { usage, .. } => Some(usage.clone()),
            _ => None,
        })
        .expect("the delegation finishes");
    let sums = |lines: &[chatty_core::models::token_usage::TokenUsage]| {
        lines.iter().fold((0, 0), |(i, o), line| {
            (i + line.input_tokens, o + line.output_tokens)
        })
    };
    assert_eq!(sums(&finished), (3_000, 110), "{finished:?}");
}

/// The issue's symptom: three solvers that each end on the answer-file
/// stop and agree. `best_of` reports their tokens, not "token use not
/// reported".
#[tokio::test]
async fn best_of_counts_solvers_ended_by_their_answer_file() {
    const SOLVERS: [(&str, &str); 3] = [
        ("kit-solver-a", "kit/solver-a"),
        ("kit-solver-b", "kit/solver-b"),
        ("kit-solver-c", "kit/solver-c"),
    ];
    let mut roster: Vec<AgentDef> = SOLVERS
        .iter()
        .map(|(name, model)| AgentDef::new(name, model, Endpoint::Sse))
        .collect();
    roster.push(AgentDef::new("kit-judge", "kit/judge", Endpoint::Ndjson));
    let script = SOLVERS.iter().fold(Script::new(), |script, (_, model)| {
        script.route(*model, ends_on_answer_file("42"))
    });
    // Each solver gets a worktree of its own: on one shared tree the first
    // to write `answer.txt` makes its siblings' write fail, and a failed
    // write does not trip the answer-file stop (one more model round).
    let kit = SwarmKit::start_in_repo_with_endpoint_budget(3, roster, script, Script::new()).await;

    let tool = BestOfTool::new(
        kit.leader_tool(),
        BestOfSection {
            solvers: SOLVERS.iter().map(|(name, _)| name.to_string()).collect(),
            judge: "kit-judge".to_string(),
        },
    );
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        tool.call(
            &mut ToolContext::new(),
            BestOfArgs {
                task: TASK.to_string(),
            },
        ),
    )
    .await
    .expect("best_of finishes before the deadline")
    .expect("best_of keeps an attempt");

    let tokens: Vec<u64> = out.attempts.iter().map(|a| a.tokens).collect();
    assert_eq!(tokens, [3_110, 3_110, 3_110], "{:?}", out.attempts);
    assert_eq!(
        out.cost,
        "3 attempts, no judge needed, 9,330 tokens (≈3.0× a single run)"
    );
}
