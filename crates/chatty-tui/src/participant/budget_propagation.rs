//! Budget propagation (PL-S2 DP-3, AGE-644) on the swarm kit: a callee
//! runs under the tighter of its own budget and what its caller has left,
//! for turns and dollars, with real worker processes behind a scripted fake
//! model. Time is `deadline_propagates`, on tokio's paused clock beside the
//! broker's call path (`chatty-protocol-gateway`, `participant/calls.rs`):
//! a worker process's clock cannot be paused, and a per-PR test waits for no
//! real deadline.
//!
//! The leader is the kit's in-process root: its `invoke_agent` with a
//! [`RunBudget`] of the test's making, as a headless leader's runner keeps
//! one current.

use chatty_core::models::token_usage::{ModelRef, TokenPricing, TokenUsage};
use chatty_core::services::run_budget::RunBudget;
use chatty_core::services::spend_gate::LocalSpendGate;
use chatty_core::settings::models::providers_store::ProviderType;
use chatty_core::testing::fake_model::{RecordedRequest, Reply, Script};
use chatty_core::tools::invoke_agent_tool::InvokeAgentProgress;

use super::swarm_kit::{AgentDef, Endpoint, SwarmKit};

/// The broker's edge log as `(kind, to, outcome, usd)`, in the order the
/// calls ended.
fn edges(kit: &SwarmKit) -> Vec<(String, String, String, Option<String>)> {
    let log = std::fs::read_to_string(kit.broker().edge_log_path()).expect("the edge log");
    log.lines()
        .map(|line| {
            let row: serde_json::Value = serde_json::from_str(line).expect("a JSON row");
            let field = |name: &str| row[name].as_str().unwrap_or_default().to_string();
            (
                field("kind"),
                field("to"),
                field("outcome"),
                row["usd"].as_str().map(str::to_string),
            )
        })
        .collect()
}

/// Whether the model was offered any tool on `request`.
fn offers_tools(request: &RecordedRequest) -> bool {
    request.json()["tools"]
        .as_array()
        .is_some_and(|tools| !tools.is_empty())
}

/// `$80` per million input tokens: a thousand input tokens cost $0.08.
fn worker_pricing() -> TokenPricing {
    TokenPricing {
        input_per_million: 80.0,
        ..TokenPricing::default()
    }
}

/// A thousand input tokens and no output, then `text`.
fn answer(text: &str) -> [Reply; 2] {
    [
        Reply::Usage {
            input: 1_000,
            output: 0,
            cache_read: 0,
        },
        Reply::text(text),
    ]
}

/// The leader's own model: priced at $20 per million input tokens.
fn leader_model() -> ModelRef {
    ModelRef {
        provider: ProviderType::OpenRouter,
        model_id: "kit/leader".to_string(),
    }
}

/// The leader's own first turn: a thousand input tokens on its model.
fn leader_turn() -> TokenUsage {
    TokenUsage {
        input_tokens: 1_000,
        model: Some(leader_model()),
        ..TokenUsage::default()
    }
}

/// The root's budget: a $0.10 `cap_usd`, priced with the kit's book and
/// the leader's own model.
fn root_budget(kit: &SwarmKit) -> RunBudget {
    let mut book = kit.price_book();
    book.insert(
        leader_model(),
        TokenPricing {
            input_per_million: 20.0,
            ..TokenPricing::default()
        },
    );
    RunBudget::new(None, LocalSpendGate::new(Some(0.10), book))
}

/// Invariant 7: with `remaining.turns = 2` the callee makes at most three
/// model calls: two tool turns and the tool-free wrap-up (TurnBudget's
/// +1). Its own spec caps nothing; left to itself it would have gone on.
#[tokio::test]
async fn turn_budget_propagates() {
    let read = || Reply::tool_call("read_file", serde_json::json!({ "path": "README.md" }));
    let list = || Reply::tool_call("list_directory", serde_json::json!({ "path": "." }));
    let kit = SwarmKit::start(
        vec![AgentDef::new("kit-w", "kit/w", Endpoint::Sse)],
        Script::new().route(
            "kit/w",
            [
                read(),
                list(),
                Reply::text("Done within two turns."),
                read(),
                Reply::text("Kept going."),
            ],
        ),
        Script::new(),
    )
    .await;

    let budget = RunBudget::new(Some(2), LocalSpendGate::default());
    let run = kit
        .run_leader_with(
            kit.leader_tool().with_run_budget(budget),
            "kit-w",
            "Look around.",
        )
        .await;
    let out = run.output.as_ref().expect("the run completes");
    assert!(out.success, "{out:?}");
    assert_eq!(out.response, "Done within two turns.");

    let calls = kit.sse.requests_for("kit/w");
    assert_eq!(calls.len(), 3, "two tool turns and the wrap-up");
    assert!(offers_tools(&calls[0]) && offers_tools(&calls[1]));
    assert!(
        !offers_tools(&calls[2]),
        "the third call is the tool-free wrap-up of a two-turn budget: {}",
        calls[2].json()
    );
}

/// Invariant 6: a $0.10 root `cap_usd` and a worker model priced in the
/// price book. The first child reports tokens and its model, no price; the
/// root prices them at the worker model, $0.08, and with its own $0.02 turn
/// the cap is spent: the second delegation is refused with
/// `budget_spent: usd` before anything is spawned.
#[tokio::test]
async fn usd_budget_propagates() {
    let kit = SwarmKit::start(
        vec![AgentDef::new("kit-w", "kit/w", Endpoint::Sse).priced(worker_pricing())],
        Script::new().route(
            "kit/w",
            answer("First answer.")
                .into_iter()
                .chain(answer("Second answer.")),
        ),
        Script::new(),
    )
    .await;
    let budget = root_budget(&kit);

    let first = kit
        .run_leader_with(
            kit.leader_tool().with_run_budget(budget.clone()),
            "kit-w",
            "First task.",
        )
        .await;
    let out = first.output.as_ref().expect("the first delegation runs");
    assert!(out.success, "{out:?}");
    let Some(InvokeAgentProgress::Finished { usage, .. }) = first.progress.last() else {
        panic!("the delegation finished: {:?}", first.progress);
    };
    assert_eq!(usage.len(), 1, "{usage:?}");
    assert_eq!(usage[0].input_tokens, 1_000);
    assert_eq!(
        usage[0].model.as_ref().map(|m| m.model_id.as_str()),
        Some("kit/w"),
        "the child names its model"
    );
    assert_eq!(usage[0].estimated_cost_usd, None, "no price on the wire");
    assert!((budget.spend().spent().usd - 0.08).abs() < 1e-9);

    // The leader's own turn after the first answer.
    budget.record([leader_turn()]);

    let second = kit
        .run_leader_with(
            kit.leader_tool().with_run_budget(budget.clone()),
            "kit-w",
            "Second task.",
        )
        .await;
    assert_eq!(
        second.output.as_ref().expect_err("the second is refused"),
        "budget_spent: usd"
    );
    assert_eq!(
        kit.participants().admitted(),
        ["kit-w-0"],
        "no second worker was admitted, so none was spawned"
    );
    assert_eq!(kit.sse.requests_for("kit/w").len(), 1);
    assert_eq!(
        edges(&kit),
        [
            (
                "task".to_string(),
                "kit-w-0".to_string(),
                "completed".to_string(),
                Some("0.080000".to_string())
            ),
            (
                "refusal".to_string(),
                "kit-w".to_string(),
                "budget_spent: usd".to_string(),
                None
            ),
        ]
    );
}

/// A line whose model the price book does not price leaves the dollar gate
/// inactive for that line: the same run as `usd_budget_propagates` on an
/// unpriced worker model spends nothing the gate counts, the second
/// delegation runs, and the edge log says `usd: unpriced`.
#[tokio::test]
async fn unpriced_model_skips_usd_gate() {
    let kit = SwarmKit::start(
        vec![AgentDef::new("kit-w", "kit/w", Endpoint::Sse)],
        Script::new().route(
            "kit/w",
            answer("First answer.")
                .into_iter()
                .chain(answer("Second answer.")),
        ),
        Script::new(),
    )
    .await;
    let budget = root_budget(&kit);

    for (task, expected) in [
        ("First task.", "First answer."),
        ("Second task.", "Second answer."),
    ] {
        let run = kit
            .run_leader_with(
                kit.leader_tool().with_run_budget(budget.clone()),
                "kit-w",
                task,
            )
            .await;
        let out = run.output.as_ref().expect("the delegation runs");
        assert!(out.success, "{out:?}");
        assert_eq!(out.response, expected);
        budget.record([leader_turn()]);
    }

    let spent = budget.spend().spent();
    assert_eq!(spent.unpriced_lines, 2, "the worker's lines are unpriced");
    assert!(
        (spent.usd - 0.04).abs() < 1e-9,
        "only the leader's own turns count"
    );
    let rows = edges(&kit);
    assert_eq!(rows.len(), 2);
    for (kind, _, outcome, usd) in &rows {
        assert_eq!((kind.as_str(), outcome.as_str()), ("task", "completed"));
        assert_eq!(usd.as_deref(), Some("unpriced"));
    }
}
