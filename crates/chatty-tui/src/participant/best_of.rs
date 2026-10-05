//! AGE-853: the `best_of` tool on the swarm kit — three real solver
//! workers and a real judge worker against the scripted fake model.
//!
//! What these pin is the runtime's half of the best-of-3 team: the task
//! reaches every solver at once and each in its own context, the judge
//! only names a candidate, and a passing verification decides before any
//! judge is asked.

use serde_json::json;

use super::swarm_kit::{AgentDef, Endpoint, SwarmKit};
use chatty_core::agent_spec::BestOfSection;
use chatty_core::testing::fake_model::{Reply, Script};
use chatty_core::tools::best_of_tool::{BestOfArgs, BestOfOutput, BestOfTool, SelectedBy};
use rig_agent::tool::{Tool, ToolContext};

const SOLVERS: [(&str, &str); 3] = [
    ("kit-solver-a", "kit/solver-a"),
    ("kit-solver-b", "kit/solver-b"),
    ("kit-solver-c", "kit/solver-c"),
];
// Not the preset's names: a preset team claims its members, and its own
// `team.json` (no verification, no isolation) would decide for them over
// the kit's module settings (AGE-763, AGE-822).
const JUDGE: &str = "kit-judge";
const JUDGE_MODEL: &str = "kit/judge";
const TASK: &str = "How many rows does payments.csv have?";

fn judge_schema() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../chatty-core/teams/crosscheck/schemas/judge.json"
    ))
    .expect("the preset's judge schema parses")
}

fn roster() -> Vec<AgentDef> {
    let mut roster: Vec<AgentDef> = SOLVERS
        .iter()
        .map(|(name, model)| AgentDef::new(name, model, Endpoint::Sse))
        .collect();
    roster.push(AgentDef::new(JUDGE, JUDGE_MODEL, Endpoint::Ndjson).with_handoff(judge_schema()));
    roster
}

/// Each solver waits `delay` ms, so all three are in flight together when
/// the fan-out is parallel, then answers with `answers[i]`.
fn solver_script(answers: [&str; 3], delay: u64) -> Script {
    SOLVERS
        .iter()
        .zip(answers)
        .fold(Script::new(), |script, ((_, model), answer)| {
            script.route(
                *model,
                [
                    Reply::Delay(delay),
                    Reply::text(format!("I counted them.\nFINAL ANSWER: {answer}")),
                ],
            )
        })
}

fn judge_picks(choice: usize) -> Script {
    Script::new().route(
        JUDGE_MODEL,
        [Reply::text(format!(
            "Candidate {choice} is right, though the true value is 99.\n\
             ```json\n{{\"choice\": {choice}, \"reason\": \"it used the right file\"}}\n```"
        ))],
    )
}

async fn best_of(kit: &SwarmKit) -> BestOfOutput {
    let tool = BestOfTool::new(
        kit.leader_tool(),
        BestOfSection {
            solvers: SOLVERS.iter().map(|(name, _)| name.to_string()).collect(),
            judge: JUDGE.to_string(),
        },
    );
    tokio::time::timeout(
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
    .expect("best_of keeps an attempt")
}

fn body(request: &chatty_core::testing::fake_model::RecordedRequest) -> String {
    String::from_utf8_lossy(&request.body).into_owned()
}

/// Three solvers, each asked once with the task as given, none shown
/// another's answer, all three in flight at once.
#[tokio::test]
async fn best_of_3_runs_three_independent_attempts() {
    let kit = SwarmKit::start_with_endpoint_budget(
        3,
        roster(),
        solver_script(["41", "42", "43"], 1_500),
        judge_picks(2),
    )
    .await;

    let out = best_of(&kit).await;

    let answers: Vec<&str> = out.attempts.iter().map(|a| a.answer.as_str()).collect();
    assert_eq!(answers, ["41", "42", "43"]);
    for (i, (_, model)) in SOLVERS.iter().enumerate() {
        let requests = kit.sse.requests_for(model);
        assert_eq!(requests.len(), 1, "{model} is asked once");
        let sent = body(&requests[0]);
        assert!(sent.contains(TASK), "{model} gets the task word for word");
        for (j, other) in ["41", "42", "43"].iter().enumerate() {
            if i != j {
                assert!(
                    !sent.contains(&format!("FINAL ANSWER: {other}")),
                    "{model} never sees another attempt"
                );
            }
        }
        assert!(!sent.contains("Candidate"), "{model} gets no judge prompt");
    }
    assert_eq!(
        kit.sse.max_concurrency(),
        3,
        "the three attempts run at once, not one after another"
    );
    assert!(out.cost.starts_with("3 attempts + judge, "), "{}", out.cost);
    assert!(out.cost.contains("× a single run"), "{}", out.cost);
}

/// The judge reads every candidate's answer and trace and names one; the
/// answer returned is that solver's, character for character, never the
/// judge's own.
#[tokio::test]
async fn judge_selects_one_attempt_and_never_rewrites() {
    let kit = SwarmKit::start_with_endpoint_budget(
        3,
        roster(),
        solver_script(["1,000", "  1 000 rows (approx.)", "999"], 0),
        judge_picks(2),
    )
    .await;

    let out = best_of(&kit).await;

    assert_eq!(out.selected_by, SelectedBy::Judge);
    assert_eq!(out.chosen, 2);
    assert_eq!(
        out.answer, "1 000 rows (approx.)",
        "solver 2's answer, verbatim"
    );
    assert!(
        !out.answer.contains("99"),
        "nothing of the judge's own text"
    );
    assert_eq!(out.reason.as_deref(), Some("it used the right file"));

    let judged = kit.ndjson.requests_for(JUDGE_MODEL);
    assert_eq!(judged.len(), 1, "the judge is asked once");
    let prompt = body(&judged[0]);
    for needle in [
        TASK,
        "Candidate 1",
        "Candidate 2",
        "Candidate 3",
        "Final answer: 1,000",
        "Final answer: 999",
        "Trace:",
    ] {
        assert!(prompt.contains(needle), "the judge sees {needle:?}");
    }
}

/// With a verification command, the one attempt whose tree passes it is
/// kept and the judge — scripted to pick another — is never asked.
#[tokio::test]
async fn verifier_overrides_judge_when_present() {
    let script = SOLVERS.iter().zip(["wrong", "right", "wrong"]).fold(
        Script::new(),
        |script, ((_, model), content)| {
            script.route(
                *model,
                [
                    Reply::tool_call(
                        "write_file",
                        json!({ "path": "answer.txt", "content": format!("{content}\n") }),
                    ),
                    Reply::text(format!("Wrote it.\nFINAL ANSWER: {content}")),
                ],
            )
        },
    );
    let kit = SwarmKit::start_verified(
        "grep -qx right answer.txt",
        3,
        roster(),
        script,
        judge_picks(1),
    )
    .await;

    let out = best_of(&kit).await;

    let verified: Vec<Option<bool>> = out.attempts.iter().map(|a| a.verified).collect();
    assert_eq!(verified, [Some(false), Some(true), Some(false)]);
    assert_eq!(out.selected_by, SelectedBy::Verifier);
    assert_eq!(out.chosen, 2);
    assert_eq!(out.answer, "right");
    assert!(
        kit.ndjson.requests_for(JUDGE_MODEL).is_empty(),
        "a verifier that decides leaves the judge unasked"
    );
    assert!(
        out.cost.starts_with("3 attempts, no judge needed"),
        "{}",
        out.cost
    );
}
