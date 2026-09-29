//! Delegation rights come from the spec, not the profile (PL-S2 DP-1,
//! AGE-642), on the swarm kit: real worker processes built from the preset
//! specs, a scripted fake model behind them.

use chatty_core::agent_spec::{AgentSpec, load_agent_spec_from};
use chatty_core::testing::fake_model::{RecordedRequest, Reply, Script};

use super::swarm_kit::{AgentDef, Endpoint, SwarmKit};

const LEADER: &str = "coder-reviewer-leader";
const CODER: &str = "local-coder";
const REVIEWER: &str = "local-reviewer";
const LEADER_MODEL: &str = "kit/leader";
const CODER_MODEL: &str = "kit/coder";
const REVIEWER_MODEL: &str = "kit/reviewer";

/// A compiled-in preset, never a spec from the developer's data dir.
/// A spec of the `coder-reviewer` test fixture (`crate::team_fixture`).
fn preset(name: &str) -> AgentSpec {
    load_agent_spec_from(name, Some(&crate::team_fixture::workspace()), None)
        .expect("the fixture spec loads")
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

fn delegates(request: &RecordedRequest) -> bool {
    let tools = tools(request);
    let invoke = tools.iter().any(|t| t == "invoke_agent");
    let list = tools.iter().any(|t| t == "list_agents");
    assert_eq!(
        invoke, list,
        "the delegation tools come as a pair: {tools:?}"
    );
    invoke
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

/// Invariant 10: the `coder-reviewer` team (now a test fixture), its three specs run as real
/// workers, delegates exactly as before — leader → coder, leader → reviewer,
/// no other edge — and only the leader is offered the delegation tools.
#[tokio::test]
async fn coder_reviewer_unchanged() {
    let team = crate::team_fixture::load();
    assert_eq!(team.leader.agent.name, LEADER);
    let names: Vec<&str> = team.agents.iter().map(|a| a.agent.name.as_str()).collect();
    assert_eq!(names, [CODER, REVIEWER]);

    let kit = SwarmKit::start(
        vec![
            AgentDef::from_spec(team.leader.clone(), LEADER_MODEL, Endpoint::Sse),
            AgentDef::from_spec(team.agents[0].clone(), CODER_MODEL, Endpoint::Ndjson),
            AgentDef::from_spec(team.agents[1].clone(), REVIEWER_MODEL, Endpoint::Ndjson),
        ],
        Script::new().route(
            LEADER_MODEL,
            [
                invoke(CODER, "Fix the overdraft check."),
                invoke(REVIEWER, "Review the coder's branch."),
                Reply::text("Merged on APPROVE."),
            ],
        ),
        Script::new()
            .route(CODER_MODEL, [Reply::text("Fixed the overdraft check.")])
            .route(REVIEWER_MODEL, [Reply::text("APPROVE")]),
    )
    .await;

    let run = kit.run_leader("Fix the overdraft bug.").await;
    let out = run.output.as_ref().expect("the team run succeeded");
    assert!(out.success, "{out:?}");
    assert_eq!(out.response, "Merged on APPROVE.");

    let leader = kit.sse.requests_for(LEADER_MODEL);
    assert_eq!(leader.len(), 3);
    assert!(
        String::from_utf8_lossy(&leader[1].body).contains("Fixed the overdraft check."),
        "the coder's answer reached the leader"
    );
    assert!(
        String::from_utf8_lossy(&leader[2].body).contains("APPROVE"),
        "the reviewer's verdict reached the leader"
    );
    assert!(delegates(&leader[0]), "the leader delegates");
    for model in [CODER_MODEL, REVIEWER_MODEL] {
        let requests = kit.ndjson.requests_for(model);
        assert_eq!(requests.len(), 1, "{model}");
        assert!(!delegates(&requests[0]), "{model} does not delegate");
    }

    let leader_node = format!("{LEADER}-0");
    assert_eq!(
        edges(&kit),
        [
            edge(&leader_node, &format!("{CODER}-0")),
            edge(&leader_node, &format!("{REVIEWER}-0")),
            edge("root", &leader_node),
        ]
    );
}

/// Invariant 11: a reviewer whose spec lists `local-coder` is offered
/// `invoke_agent` on the `reviewer` profile, hands the fix to the coder and
/// receives its answer.
#[tokio::test]
async fn reviewer_delegates_back() {
    let mut reviewer = preset(REVIEWER);
    assert_eq!(reviewer.tools.profile.as_deref(), Some("reviewer"));
    reviewer.swarm.delegates_to = vec![CODER.to_string()];

    let kit = SwarmKit::start(
        vec![
            AgentDef::from_spec(reviewer, REVIEWER_MODEL, Endpoint::Sse),
            AgentDef::from_spec(preset(CODER), CODER_MODEL, Endpoint::Ndjson),
        ],
        Script::new().route(
            REVIEWER_MODEL,
            [
                invoke(CODER, "Fix: the overdraft check is off by one."),
                Reply::text("APPROVE after the coder's fix."),
            ],
        ),
        Script::new().route(CODER_MODEL, [Reply::text("Fixed the off-by-one.")]),
    )
    .await;

    let run = kit.run_leader("Review the branch.").await;
    let out = run.output.as_ref().expect("the review succeeded");
    assert!(out.success, "{out:?}");
    assert_eq!(out.response, "APPROVE after the coder's fix.");

    let reviewer = kit.sse.requests_for(REVIEWER_MODEL);
    assert_eq!(reviewer.len(), 2);
    assert!(
        delegates(&reviewer[0]),
        "delegates_to gives the reviewer invoke_agent"
    );
    assert!(
        !tools(&reviewer[0]).iter().any(|t| t == "write_file"),
        "it is still a reviewer"
    );
    let coder = kit.ndjson.requests_for(CODER_MODEL);
    assert_eq!(coder.len(), 1);
    assert!(
        String::from_utf8_lossy(&coder[0].body).contains("off by one"),
        "the coder got the fix"
    );
    assert!(
        String::from_utf8_lossy(&reviewer[1].body).contains("Fixed the off-by-one."),
        "the coder's answer reached the reviewer"
    );

    let reviewer_node = format!("{REVIEWER}-0");
    assert_eq!(
        edges(&kit),
        [
            edge(&reviewer_node, &format!("{CODER}-0")),
            edge("root", &reviewer_node),
        ]
    );
}
