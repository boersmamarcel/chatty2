//! TD-1 (AGE-692): a team's derived single agent, and the scorecard row a
//! run writes, with its failure tags (fabric-team-design invariants 1, 6
//! and 7).
//!
//! The scorecard runs are on the swarm kit: real workers behind the fake
//! model, the broker's real edge log. As in `swarm_trace.rs`, the kit's
//! leader is the in-process root's `invoke_agent` call, so the root's own
//! events around its calls are scripted the way `AgentSession` emits them.
//!
//! `UPDATE_GOLDENS=1` rewrites the derived specs' goldens.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chatty_core::models::token_usage::{ModelRef, PriceBook, TokenPricing, TokenUsage, price};
use chatty_core::services::handoff::{HandoffContract, HandoffLedger};
use chatty_core::services::swarm_trace::SwarmTrace;
use chatty_core::services::team::load_team;
use chatty_core::services::team_scorecard::{
    Arm, HANDOFF_MISREAD, NEVER_DELEGATED, RunRecord, ScorecardRow, VERIFICATION_PASSED_WRONG,
    WRONG_ROLE_CALLED, derive_single,
};
use chatty_core::session::SessionEvent;
use chatty_core::settings::models::providers_store::ProviderType;
use chatty_core::testing::fake_model::{Reply, Script};
use chatty_core::tools::invoke_agent_tool::InvokeAgentProgress;
use serde_json::{Value, json};

use super::swarm_kit::{AgentDef, Endpoint, SwarmKit};

const LEADER: &str = "kit-lead";
const LEADER_MODEL: &str = "kit/lead";
const CODER: &str = "kit-coder";
const CODER_MODEL: &str = "kit/coder";
const REVIEWER: &str = "kit-reviewer";
const REVIEWER_MODEL: &str = "kit/reviewer";
const SCOUT: &str = "kit-scout";
const SCOUT_MODEL: &str = "kit/scout";

/// The kit team's skill: it names the coder and the reviewer, not the scout.
const PLAYBOOK: &str = "1. Delegate the change to kit-coder.\n\
                        2. Have kit-reviewer check it; its first line is APPROVE or \
                        REQUEST_CHANGES.";

const KIT_TEAM: Arm<'static> = Arm::Team {
    id: "kit-team",
    leader: LEADER,
    playbook: Some(PLAYBOOK),
};

fn golden_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/goldens")
        .join(name)
}

fn check_golden(name: &str, actual: &str) {
    let path = golden_path(name);
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{} is missing ({e}); run with UPDATE_GOLDENS=1",
            path.display()
        )
    });
    assert_eq!(
        actual,
        expected,
        "{} changed; if that is intended, rerun with UPDATE_GOLDENS=1",
        path.display()
    );
}

/// Invariant 1: the baseline is derived, not written. `coder-reviewer` (the
/// test fixture; it no longer ships) and the `crosscheck` preset each derive
/// a valid spec that matches its golden. `analyst-swarm` is not on main yet
/// (AGE-668), so `crosscheck` stands in for it.
#[test]
fn single_arm_is_derived_from_team() {
    let teams = [
        crate::team_fixture::load(),
        load_team("crosscheck", None, None).expect("the crosscheck preset loads"),
    ];
    for team in &teams {
        let single = derive_single(team, &[], &PriceBook::default());
        single
            .validate(None)
            .unwrap_or_else(|e| panic!("{}'s single agent: {e}", team.id));
        check_golden(
            &format!("single_{}.toml", team.id.replace('-', "_")),
            &single.to_toml().expect("the spec serializes"),
        );
    }
}

/// `(input, output, cache_read)` the fake server reports for the next
/// answer.
fn usage(input: u64, output: u64) -> Reply {
    Reply::Usage {
        input,
        output,
        cache_read: 0,
    }
}

fn pricing(input: f64, output: f64) -> TokenPricing {
    TokenPricing {
        input_per_million: input,
        output_per_million: output,
        ..TokenPricing::default()
    }
}

fn leader_model() -> ModelRef {
    ModelRef {
        provider: ProviderType::Ollama,
        model_id: LEADER_MODEL.to_string(),
    }
}

/// The kit's prices with the leader's model in them too.
fn price_book(kit: &SwarmKit) -> PriceBook {
    let mut book = kit.price_book();
    book.set(leader_model(), pricing(1.0, 4.0));
    book
}

/// One finished team run: its trace, and the usage lines the root's
/// conversation records for it (its own, then each delegation's).
struct Played {
    trace: SwarmTrace,
    bill: Vec<TokenUsage>,
}

/// The root runs one turn: it delegates each `(agent, prompt)` in order
/// through the real `invoke_agent` (recording handoffs in `ledger`), then
/// answers.
async fn play(kit: &SwarmKit, ledger: Option<&HandoffLedger>, calls: &[(&str, &str)]) -> Played {
    let leader_line = TokenUsage {
        model: Some(leader_model()),
        ..TokenUsage::new(50, 10)
    };
    let mut events = vec![SessionEvent::TurnStarted];
    let mut bill = vec![leader_line.clone()];
    for (n, (agent, prompt)) in calls.iter().enumerate() {
        let tool = match ledger {
            Some(ledger) => kit.leader_tool().with_handoff_ledger(ledger.clone()),
            None => kit.leader_tool(),
        };
        let run = kit.run_leader_with(tool, agent, prompt).await;
        let out = run.output.expect("the delegation succeeded");
        let id = format!("lead-{n}");
        events.push(SessionEvent::ToolCallStarted {
            id: id.clone(),
            name: "invoke_agent".into(),
        });
        events.push(SessionEvent::ToolCallInput {
            id: id.clone(),
            arguments: json!({ "agent": agent, "prompt": prompt }).to_string(),
        });
        events.extend(run.progress.into_iter().map(|p| match p {
            InvokeAgentProgress::Swarm(batch) => SessionEvent::SwarmEvent(batch),
            p => SessionEvent::Delegation(p),
        }));
        events.push(SessionEvent::ToolCallResult {
            id,
            result: serde_json::to_string(&out).expect("the result serializes"),
        });
        bill.extend(out.usage.iter().cloned());
    }
    events.extend([
        SessionEvent::Text("Done.".into()),
        SessionEvent::TokenUsage(leader_line),
        SessionEvent::TurnEnded,
    ]);
    // No call, no edge log.
    let log = std::fs::read_to_string(kit.broker().edge_log_path()).unwrap_or_default();
    let edges: Vec<chatty_fabric::EdgeRow> = log
        .lines()
        .map(|line| serde_json::from_str(line).expect("an edge row"))
        .collect();
    Played {
        trace: SwarmTrace::from_edges(&edges, &events),
        bill,
    }
}

fn row(
    arm: Arm<'_>,
    played: &Played,
    ledger: Option<&HandoffLedger>,
    solved: bool,
) -> ScorecardRow {
    ScorecardRow::new(&RunRecord {
        run: "td1-kit",
        task: "c01-pagination",
        arm,
        trace: &played.trace,
        handoffs: ledger,
        solved,
        wall: Duration::from_millis(42_700),
    })
}

/// A coder, then a reviewer that answers `verdict`, both priced.
async fn coder_reviewer_kit(verdict: &str) -> SwarmKit {
    SwarmKit::start(
        vec![
            AgentDef::new(CODER, CODER_MODEL, Endpoint::Sse).priced(pricing(2.0, 8.0)),
            AgentDef::new(REVIEWER, REVIEWER_MODEL, Endpoint::Ndjson).priced(pricing(0.5, 1.5)),
        ],
        Script::new().route(
            CODER_MODEL,
            [
                usage(400, 30),
                Reply::tool_call("read_file", json!({ "path": "README.md" })),
                usage(600, 80),
                Reply::text("Changed it."),
            ],
        ),
        Script::new().route(REVIEWER_MODEL, [usage(300, 20), Reply::text(verdict)]),
    )
    .await
}

const CALLS: [(&str, &str); 2] = [
    (CODER, "change the readme"),
    (REVIEWER, "check the coder's change"),
];

/// Invariant 6: the scorecard adds up. A row's lines, priced on read, cost
/// what AGE-682's priced view of the same run's lines does: the root's own
/// line and each delegation's, as the conversation records them. The row
/// stores the lines, never a priced sum.
#[tokio::test]
async fn scorecard_cost_matches_bill() {
    let kit = coder_reviewer_kit("APPROVE").await;
    let played = play(&kit, None, &CALLS).await;
    let book = price_book(&kit);
    let row = row(KIT_TEAM, &played, None, true);

    let bill = price(&played.bill, &book);
    let cost = row.priced(&book);
    assert_eq!((bill.unpriced_lines, cost.unpriced_lines), (0, 0));
    assert!(bill.usd > 0.0);
    assert!(
        (cost.usd - bill.usd).abs() < 1e-12,
        "row {} vs bill {}",
        cost.usd,
        bill.usd
    );

    assert_eq!(row.arm, "kit-team");
    assert_eq!(row.wall_s, 42);
    assert_eq!(
        row.turns_by_role
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [CODER, LEADER, REVIEWER]
    );
    assert!(row.failure_tags.is_empty(), "{:?}", row.failure_tags);
    let line = serde_json::to_string(&row).expect("the row serializes");
    assert!(!line.contains("usd"), "no stored price: {line}");
    assert_eq!(
        serde_json::from_str::<ScorecardRow>(&line).expect("the row parses"),
        row
    );
}

/// The coder's and the reviewer's handoff schemas: the reviewer must quote
/// every file the coder changed (TD-2's read rules).
fn handoff_contracts() -> [(Value, HandoffContract); 2] {
    let change = json!({
        "type": "object",
        "required": ["files_changed"],
        "properties": { "files_changed": { "type": "array", "items": { "type": "string" } } }
    });
    let review = json!({
        "type": "object",
        "required": ["reviewed"],
        "properties": { "reviewed": { "type": "array", "items": { "type": "string" } } },
        "x-must-be-read": { "kit-coder": ["files_changed"] }
    });
    [(CODER, change), (REVIEWER, review)].map(|(role, schema)| {
        let contract = HandoffContract {
            role: role.to_string(),
            schema: chatty_fabric::wire::Opaque::from_value(&schema).unwrap(),
        };
        (schema, contract)
    })
}

/// Invariant 7: each failure tag comes from exactly one scripted scenario
/// and from no other. The single arm, which cannot delegate, earns none,
/// and an approval the check agrees with is no failure.
#[tokio::test]
async fn failure_tags_are_grounded() {
    // never_delegated: the leader answers on its own.
    let kit = coder_reviewer_kit("APPROVE").await;
    let alone = play(&kit, None, &[]).await;
    assert_eq!(
        row(KIT_TEAM, &alone, None, false).failure_tags,
        [NEVER_DELEGATED]
    );
    let single = Arm::Single {
        spec: "kit-team-single",
    };
    let single_row = row(single, &alone, None, false);
    assert_eq!(single_row.arm, "single");
    assert!(
        single_row.failure_tags.is_empty(),
        "{:?}",
        single_row.failure_tags
    );

    // verification_passed_wrong: the reviewer approves, the check fails.
    let approved = play(&kit, None, &CALLS).await;
    assert_eq!(
        row(KIT_TEAM, &approved, None, false).failure_tags,
        [VERIFICATION_PASSED_WRONG]
    );
    assert!(row(KIT_TEAM, &approved, None, true).failure_tags.is_empty());
    drop(kit);

    // wrong_role_called: the leader asks the scout, whom its skill never
    // names.
    let kit = SwarmKit::start(
        vec![
            AgentDef::new(CODER, CODER_MODEL, Endpoint::Sse),
            AgentDef::new(SCOUT, SCOUT_MODEL, Endpoint::Sse),
        ],
        Script::new().route(SCOUT_MODEL, [Reply::text("The readme says Chatty.")]),
        Script::new(),
    )
    .await;
    let scouted = play(&kit, None, &[(SCOUT, "look around")]).await;
    assert_eq!(
        row(KIT_TEAM, &scouted, None, false).failure_tags,
        [WRONG_ROLE_CALLED]
    );
    drop(kit);

    // handoff_misread: the reviewer's handoff leaves out a file the coder
    // changed, and it asks for changes rather than approving.
    let [(change, coder_contract), (review, reviewer_contract)] = handoff_contracts();
    let kit = SwarmKit::start(
        vec![
            AgentDef::new(CODER, CODER_MODEL, Endpoint::Sse).with_handoff(change),
            AgentDef::new(REVIEWER, REVIEWER_MODEL, Endpoint::Sse).with_handoff(review),
        ],
        Script::new()
            .route(
                CODER_MODEL,
                [Reply::text(
                    "Changed both.\n\n```json\n{\"files_changed\": [\"README.md\", \"calc.sh\"]}\n```",
                )],
            )
            .route(
                REVIEWER_MODEL,
                [Reply::text(
                    "REQUEST_CHANGES\n1. Add a test.\n\n```json\n{\"reviewed\": [\"README.md\"]}\n```",
                )],
            ),
        Script::new(),
    )
    .await;
    let ledger = HandoffLedger::new([&coder_contract, &reviewer_contract]);
    let misread = play(&kit, Some(&ledger), &CALLS).await;
    let misread_row = row(KIT_TEAM, &misread, Some(&ledger), false);
    assert_eq!(misread_row.failure_tags, [HANDOFF_MISREAD]);
    assert!(misread_row.handoff_invalid_by_role.is_empty());
}
