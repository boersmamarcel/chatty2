//! AGE-998: `/agent crosscheck-lead` without `--team`. The root hands the
//! task to the `crosscheck-lead` preset, which the broker starts as a real
//! `chatty-tui` worker with `--agent-json` and no team. Its solvers and its
//! judge are callers-only members (`callers = ["crosscheck-lead", …]`);
//! the lead is offered them, and its `best_of` reaches every one through the
//! broker, whose delegation policy allows the calls.

use chatty_core::agent_spec::{AgentSpec, load_agent_spec_from};
use chatty_core::testing::fake_model::{Reply, Script};

use super::swarm_kit::{AgentDef, Endpoint, SwarmKit};

const LEAD_MODEL: &str = "kit/crosscheck-lead";
const JUDGE_MODEL: &str = "kit/crosscheck-judge";
const SOLVERS: [(&str, &str); 3] = [
    ("crosscheck-solver-direct", "kit/solver-direct"),
    ("crosscheck-solver-plan", "kit/solver-plan"),
    ("crosscheck-solver-verify", "kit/solver-verify"),
];
const TASK: &str = "How many rows does payments.csv have?";

/// A compiled-in preset, never a spec from the developer's data dir.
fn preset(name: &str) -> AgentSpec {
    load_agent_spec_from(name, None, None)
        .expect("the preset loads")
        .spec
}

/// The broker's edge log as `(from, to, outcome)` for every task edge.
fn task_edges(kit: &SwarmKit) -> Vec<(String, String, String)> {
    let log = std::fs::read_to_string(kit.broker().edge_log_path()).expect("the edge log");
    log.lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("a JSON row"))
        .filter(|row| row["kind"] == "task")
        .map(|row| {
            let field = |name: &str| row[name].as_str().unwrap_or_default().to_string();
            (field("from"), field("to"), field("outcome"))
        })
        .collect()
}

#[tokio::test]
async fn crosscheck_lead_without_a_team_reaches_its_callers_only_members() {
    for (name, _) in SOLVERS.iter().chain([&("crosscheck-judge", "")]) {
        assert!(
            preset(name).swarm.callers.is_some(),
            "{name} is a callers-only member"
        );
    }
    let mut roster = vec![AgentDef::from_spec(
        preset("crosscheck-lead"),
        LEAD_MODEL,
        Endpoint::Sse,
    )];
    roster.extend(
        SOLVERS
            .iter()
            .map(|(name, model)| AgentDef::from_spec(preset(name), model, Endpoint::Ndjson)),
    );
    roster.push(AgentDef::from_spec(
        preset("crosscheck-judge"),
        JUDGE_MODEL,
        Endpoint::Ndjson,
    ));
    let solvers = SOLVERS
        .iter()
        .zip(["41", "42", "43"])
        .fold(Script::new(), |script, ((_, model), answer)| {
            script.route(
                *model,
                [Reply::text(format!(
                    "I counted them.\nFINAL ANSWER: {answer}"
                ))],
            )
        })
        .route(
            JUDGE_MODEL,
            [Reply::text(
                "Candidate 2 is right.\n\
                 ```json\n{\"choice\": 2, \"reason\": \"it read the right file\"}\n```",
            )],
        );
    let kit = SwarmKit::start_with_endpoint_budget(
        4,
        roster,
        Script::new().route(
            LEAD_MODEL,
            [
                Reply::tool_call("best_of", serde_json::json!({ "task": TASK })),
                Reply::text("FINAL ANSWER: 42"),
            ],
        ),
        solvers,
    )
    .await;

    // The root's `/agent crosscheck-lead`: the first agent in the roster.
    let run = kit.run_leader(TASK).await;
    let out = run.output.as_ref().expect("the lead's run succeeded");
    assert!(out.success, "{out:?}");

    let lead_requests = kit.sse.requests_for(LEAD_MODEL);
    assert_eq!(lead_requests.len(), 2, "best_of, then the answer");
    let after_best_of = String::from_utf8_lossy(&lead_requests[1].body).into_owned();
    assert!(
        after_best_of.contains("it read the right file"),
        "best_of reached the solvers and the judge: {after_best_of}"
    );

    // Every member ran, once, for the lead and nobody else.
    for (_, model) in SOLVERS {
        assert_eq!(kit.ndjson.requests_for(model).len(), 1, "{model}");
    }
    assert_eq!(kit.ndjson.requests_for(JUDGE_MODEL).len(), 1);
    let edges = task_edges(&kit);
    let lead_node = "crosscheck-lead-0";
    for member in SOLVERS
        .iter()
        .map(|(name, _)| *name)
        .chain(["crosscheck-judge"])
    {
        assert!(
            edges.iter().any(|(from, to, outcome)| from == lead_node
                && to == &format!("{member}-0")
                && outcome == "completed"),
            "{lead_node} → {member}: {edges:?}"
        );
    }
    assert!(
        edges
            .iter()
            .any(|(from, to, _)| from == "root" && to == lead_node),
        "{edges:?}"
    );
}
