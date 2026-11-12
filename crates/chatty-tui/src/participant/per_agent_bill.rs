//! TB-3 (AGE-665) on the swarm kit: one bill line per agent in the tree,
//! not one per direct child, and totals unchanged.
//!
//! The root is a real `AgentSession` whose own model asks the reviewer; the
//! reviewer asks the coder, and the coder asks the tester. Every level is a
//! real worker on the fake model. The coder runs on the tester's model, so
//! the coder's own spend and the tester's are on one model: a sub-leader
//! that merged lines on the model alone would fold the tester's line into
//! the coder's (or the other way round), and the paths would show it.

use std::cell::RefCell;
use std::rc::Rc;

use chatty_core::models::token_usage::{ModelRef, PriceBook, TokenPricing, TokenUsage, price};
use chatty_core::session::{SessionEvent, TurnInput};
use chatty_core::settings::models::providers_store::ProviderType;
use chatty_core::testing::fake_model::{Reply, Script};
use serde_json::json;

use super::slash_agent::{ROOT_MODEL, root_session_with};
use super::swarm_kit::{AgentDef, Endpoint, SwarmKit};

const REVIEWER: &str = "kit-reviewer";
const REVIEWER_MODEL: &str = "kit/reviewer";
const CODER: &str = "kit-coder";
const TESTER: &str = "kit-tester";
/// The model the coder and the tester both run on.
const SHARED_MODEL: &str = "kit/shared";

/// `(input, output)` of each agent's two answers: its delegation (or its
/// first look), then its reply.
const ROOT_USAGE: [(u64, u64); 2] = [(1_000, 10), (1_500, 20)];
const REVIEWER_USAGE: [(u64, u64); 2] = [(2_000, 30), (2_500, 40)];
const CODER_USAGE: [(u64, u64); 2] = [(3_000, 50), (3_500, 60)];
const TESTER_USAGE: [(u64, u64); 2] = [(4_000, 70), (4_500, 80)];

fn usage((input, output): (u64, u64)) -> Reply {
    Reply::Usage {
        input,
        output,
        cache_read: 0,
    }
}

/// An agent's two answers: ask `next` (or read the readme), then `reply`.
fn answers(spent: [(u64, u64); 2], next: Option<&str>, reply: &str) -> Vec<Reply> {
    let first = match next {
        Some(agent) => Reply::tool_call(
            "invoke_agent",
            json!({ "agent": agent, "prompt": "pass it on" }),
        ),
        None => Reply::tool_call("read_file", json!({ "path": "README.md" })),
    };
    vec![usage(spent[0]), first, usage(spent[1]), Reply::text(reply)]
}

fn pricing(input: f64, output: f64) -> TokenPricing {
    TokenPricing {
        input_per_million: input,
        output_per_million: output,
        ..TokenPricing::default()
    }
}

fn root_model() -> ModelRef {
    ModelRef {
        provider: ProviderType::OpenRouter,
        model_id: ROOT_MODEL.to_string(),
    }
}

/// What `spent` costs at `rates`, in USD.
fn cost(spent: [(u64, u64); 2], rates: (f64, f64)) -> f64 {
    spent
        .iter()
        .map(|(input, output)| (*input as f64 * rates.0 + *output as f64 * rates.1) / 1e6)
        .sum()
}

fn tokens(spent: [(u64, u64); 2]) -> (u32, u32) {
    (
        spent.iter().map(|(i, _)| *i as u32).sum(),
        spent.iter().map(|(_, o)| *o as u32).sum(),
    )
}

/// The lines merged per model, as a sub-leader reported them before TB-3:
/// AGE-682's view of what was spent on which model, with no agent.
fn per_model(lines: &[TokenUsage]) -> Vec<TokenUsage> {
    let mut merged: Vec<TokenUsage> = Vec::new();
    for line in lines {
        match merged.iter_mut().find(|known| known.model == line.model) {
            Some(known) => {
                known.input_tokens += line.input_tokens;
                known.output_tokens += line.output_tokens;
            }
            None => merged.push(TokenUsage {
                input_tokens: line.input_tokens,
                output_tokens: line.output_tokens,
                model: line.model.clone(),
                ..TokenUsage::default()
            }),
        }
    }
    merged
}

/// AGE-665's acceptance test: a three-level run gives the root three
/// delegated lines, one per agent below it, named by its path down the
/// tree — the coder's own line and the tester's apart, though both are on
/// one model; the totals are AGE-682's priced view of the same lines, and of the
/// same spend folded per model; `last_usage` and the context fill stay the
/// root's own.
#[tokio::test]
async fn per_agent_bill_lines_total_unchanged() {
    // The root's, the reviewer's, and the shared model's prices.
    const RATES: [(f64, f64); 3] = [(1.0, 5.0), (3.0, 15.0), (10.0, 40.0)];
    let kit = SwarmKit::start(
        vec![
            AgentDef::new(REVIEWER, REVIEWER_MODEL, Endpoint::Sse)
                .sub_leader()
                .priced(pricing(RATES[1].0, RATES[1].1)),
            AgentDef::new(CODER, SHARED_MODEL, Endpoint::Sse)
                .sub_leader()
                .priced(pricing(RATES[2].0, RATES[2].1)),
            AgentDef::new(TESTER, SHARED_MODEL, Endpoint::Sse)
                .priced(pricing(RATES[2].0, RATES[2].1)),
        ],
        Script::new()
            .route(
                ROOT_MODEL,
                answers(ROOT_USAGE, Some(REVIEWER), "The reviewer is done."),
            )
            .route(
                REVIEWER_MODEL,
                answers(REVIEWER_USAGE, Some(CODER), "The coder is done."),
            )
            // One model, served in arrival order: the coder asks the
            // tester, the tester answers twice, then the coder answers.
            .route(SHARED_MODEL, {
                let coder = answers(CODER_USAGE, Some(TESTER), "The tester is done.");
                let tester = answers(TESTER_USAGE, None, "It says Chatty.");
                [&coder[..2], &tester[..], &coder[2..]].concat()
            }),
        Script::new(),
    )
    .await;
    let mut book: PriceBook = kit.price_book();
    book.insert(root_model(), pricing(RATES[0].0, RATES[0].1));
    let mut session = root_session_with(&kit, &[REVIEWER, CODER, TESTER]).await;
    session.set_price_book(book.clone());

    let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
    let sink = events.clone();
    let turn = session
        .begin_turn(TurnInput::text("Have it reviewed."), move |event| {
            sink.borrow_mut().push(event)
        })
        .expect("the root's turn starts");
    tokio::time::timeout(std::time::Duration::from_secs(120), turn)
        .await
        .expect("the root's turn ends before the deadline");
    let events = Rc::try_unwrap(events).unwrap().into_inner();
    // As an owner does: apply the turn's events, then finish it.
    for event in &events {
        session.apply(event);
    }
    assert!(
        session.finish_turn(None, Vec::new()).is_some(),
        "the turn is persisted: {events:#?}"
    );
    let shared = kit.sse.requests_for(SHARED_MODEL).len();
    assert_eq!(shared, 4, "the coder and the tester ran: {events:#?}");

    let conversation = session.conversation().expect("the root's conversation");
    let usage = conversation.token_usage();
    let lines = &usage.message_usages;

    // One delegated line per agent in the tree, each named by its path.
    let delegated: Vec<(&str, (u32, u32), Option<&str>)> = lines
        .iter()
        .filter_map(|line| {
            Some((
                line.delegated_to.as_deref()?,
                (line.input_tokens, line.output_tokens),
                line.model.as_ref().map(|m| m.model_id.as_str()),
            ))
        })
        .collect();
    let reviewer = REVIEWER.to_string();
    let coder = format!("{REVIEWER}/{CODER}");
    let tester = format!("{REVIEWER}/{CODER}/{TESTER}");
    let mut sorted = delegated.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        vec![
            (
                reviewer.as_str(),
                tokens(REVIEWER_USAGE),
                Some(REVIEWER_MODEL)
            ),
            (coder.as_str(), tokens(CODER_USAGE), Some(SHARED_MODEL)),
            (tester.as_str(), tokens(TESTER_USAGE), Some(SHARED_MODEL)),
        ],
        "three delegated lines, one per agent: {lines:#?}"
    );
    assert_eq!(lines.len(), 4, "three delegated lines and the root's own");
    // The coder's own spend on the shared model is its own line, the
    // tester's not folded into it.
    let on_shared: Vec<(&str, (u32, u32))> = delegated
        .iter()
        .filter(|(_, _, model)| *model == Some(SHARED_MODEL))
        .map(|(path, spent, _)| (*path, *spent))
        .collect();
    assert_eq!(on_shared.len(), 2, "{lines:#?}");
    assert!(
        on_shared.contains(&(coder.as_str(), tokens(CODER_USAGE))),
        "the coder's own line on the shared model: {lines:#?}"
    );

    // `last_usage` and the context fill are the root's own.
    let own = usage.last_usage().expect("the root's own line");
    assert_eq!(own.delegated_to, None);
    assert_eq!((own.input_tokens, own.output_tokens), tokens(ROOT_USAGE));
    assert_eq!(conversation.context_tokens(), ROOT_USAGE[1].0 as u32);

    // The totals are AGE-682's priced view of the same lines…
    let priced = price(lines, &book);
    assert_eq!(priced.unpriced_lines, 0, "{lines:#?}");
    assert!(
        (usage.total_estimated_cost_usd - priced.usd).abs() < 1e-12,
        "total {} vs priced {}",
        usage.total_estimated_cost_usd,
        priced.usd
    );
    // …and of the same spend folded per model, as it was reported before…
    let folded = price(&per_model(lines), &book);
    assert!((usage.total_estimated_cost_usd - folded.usd).abs() < 1e-12);
    // …and are exactly what the fake server served, at each model's rates.
    let served = cost(ROOT_USAGE, RATES[0])
        + cost(REVIEWER_USAGE, RATES[1])
        + cost(CODER_USAGE, RATES[2])
        + cost(TESTER_USAGE, RATES[2]);
    assert!(
        (usage.total_estimated_cost_usd - served).abs() < 1e-12,
        "total {} vs served {served}",
        usage.total_estimated_cost_usd
    );
    let all = [ROOT_USAGE, REVIEWER_USAGE, CODER_USAGE, TESTER_USAGE];
    let (input, output) = all.iter().fold((0, 0), |(i, o), spent| {
        let (a, b) = tokens(*spent);
        (i + a, o + b)
    });
    assert_eq!(
        (usage.total_input_tokens, usage.total_output_tokens),
        (input, output)
    );
}
