//! AGE-903: a spent budget ends the whole swarm cleanly.
//!
//! The model endpoint refuses a call with `402` and a body naming
//! `cap_exceeded` (hive's egress today) or `budget_exceeded`. Every hop is
//! real: the root is the kit's in-process root (its leader tool), every
//! worker a `chatty-tui` process behind the kit's broker, and the refusal
//! is the fake model's scripted `402`.

use chatty_core::services::budget_stop::BUDGET_STOP_NOTE;
use chatty_core::testing::fake_model::{Reply, Script};
use chatty_core::tools::invoke_agent_tool::{
    InvokeAgentArgs, InvokeAgentError, InvokeAgentOutput, InvokeAgentProgress, InvokeAgentTool,
};
use chatty_fabric::CapturedConversation;
use rig_agent::tool::{Tool, ToolContext};

use super::swarm_kit::{AgentDef, Endpoint, SwarmKit};

const WORKER: &str = "kit-worker";
const WORKER_MODEL: &str = "kit/worker";
const SLOW: &str = "kit-slow";
const SLOW_MODEL: &str = "kit/slow";
const LEAD: &str = "kit-lead";
const LEAD_MODEL: &str = "kit/lead";

/// Generous for CI: only a failure waits this long.
const DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// A model call that does not come back while the test runs.
const HANG_MS: u64 = 120_000;

/// hive's egress refusal, as `routes/egress.rs` answers it today.
fn cap_exceeded() -> Reply {
    Reply::ErrorBody(
        402,
        serde_json::json!({ "error": "cap_exceeded", "month_to_date": 12.5, "cap_usd": 10.0 }),
    )
}

fn read_readme() -> Reply {
    Reply::tool_call("read_file", serde_json::json!({ "path": "README.md" }))
}

/// `tool`'s `invoke_agent` of `agent`, bounded by the deadline.
async fn invoke(
    tool: &InvokeAgentTool,
    agent: &str,
    prompt: &str,
) -> Result<InvokeAgentOutput, InvokeAgentError> {
    tokio::time::timeout(
        DEADLINE,
        tool.call(
            &mut ToolContext::new(),
            InvokeAgentArgs {
                agent: agent.to_string(),
                prompt: prompt.to_string(),
                include_trace: false,
            },
        ),
    )
    .await
    .expect("the delegation ends before the deadline")
}

/// Wait, up to the deadline, until `done` holds.
async fn until(what: &str, mut done: impl FnMut() -> bool) {
    tokio::time::timeout(DEADLINE, async {
        while !done() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what} before the deadline"));
}

/// The refused call is terminal: classified as a spent budget, never
/// retried (a `ProviderStatus` would be, after 10 s, on the worker's
/// headless surface), and the leader reads the typed stop.
#[tokio::test]
async fn budget_402_is_terminal_not_retried() {
    let kit = SwarmKit::start(
        vec![AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Sse)],
        Script::new().route(
            WORKER_MODEL,
            [
                cap_exceeded(),
                Reply::text("unreachable: the call was retried"),
            ],
        ),
        Script::new(),
    )
    .await;

    let run = kit.run_leader("read the readme").await;

    let error = run.output.as_ref().expect_err("the budget stopped it");
    assert!(
        error.contains("budget_exhausted: Budget of $10.00 spent"),
        "{error}"
    );
    assert!(error.contains(BUDGET_STOP_NOTE), "{error}");
    assert!(!error.contains("unreachable"), "{error}");
    assert_eq!(
        kit.sse.requests_for(WORKER_MODEL).len(),
        1,
        "the refused call is not retried"
    );
}

/// A worker that is refused mid-run ends with a terminal `budget_exhausted`
/// status, and the leader gets what it had done: the text it streamed, its
/// usage, and its captured conversation (RC-0).
#[tokio::test]
async fn worker_budget_exhausted_reaches_leader_with_partial_output() {
    let kit = SwarmKit::start(
        vec![AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Sse)],
        Script::new().route(
            WORKER_MODEL,
            [
                Reply::Say("Checking the README first.".to_string()),
                read_readme(),
                // The shape AGE-902's egress may send: a `code`, a message
                // and the task's budget.
                Reply::ErrorBody(
                    402,
                    serde_json::json!({
                        "code": "budget_exceeded",
                        "message": "task budget spent",
                        "budget_usd": 0.5,
                    }),
                ),
            ],
        ),
        Script::new(),
    )
    .await;

    let run = kit.run_leader("read the readme").await;

    let error = run.output.as_ref().expect_err("the budget stopped it");
    assert!(
        error.contains("budget_exhausted: Budget of $0.50 spent"),
        "{error}"
    );
    assert!(
        error.contains("What 'kit-worker' finished:\nChecking the README first."),
        "the partial output reaches the leader: {error}"
    );
    assert_eq!(kit.sse.requests_for(WORKER_MODEL).len(), 2);

    let usage = run
        .progress
        .iter()
        .find_map(|p| match p {
            InvokeAgentProgress::Finished {
                success: false,
                usage,
                ..
            } => Some(usage.clone()),
            _ => None,
        })
        .expect("the delegation finished");
    assert!(
        usage.iter().map(|line| line.input_tokens).sum::<u32>() >= 10,
        "the worker's spend reaches the leader: {usage:?}"
    );

    let conversation = run
        .progress
        .iter()
        .find_map(|p| match p {
            InvokeAgentProgress::Conversation(CapturedConversation::Messages { messages }) => {
                Some(messages.to_string())
            }
            _ => None,
        })
        .expect("the worker's conversation is captured at its end");
    assert!(
        conversation.contains("# Chatty"),
        "the read it did is kept: {conversation}"
    );
}

/// One delegation's budget stop stops its siblings: the one still running
/// hangs up — its worker is reaped, as a stopped call's is (TB-7) — and
/// the run starts no other.
#[tokio::test]
async fn leader_cancels_inflight_siblings_on_budget_exhausted() {
    let kit = SwarmKit::start(
        vec![
            AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Sse),
            AgentDef::new(SLOW, SLOW_MODEL, Endpoint::Ndjson),
        ],
        Script::new().route(WORKER_MODEL, [cap_exceeded()]),
        Script::new().route(
            SLOW_MODEL,
            [
                Reply::Delay(HANG_MS),
                Reply::text("slow: should not finish"),
            ],
        ),
    )
    .await;
    let tool = kit.leader_tool();

    // Side by side, as rig runs a leader's parallel calls: the slow sibling
    // is inside its model call when the other is refused.
    let slow = invoke(&tool, SLOW, "take your time");
    let refused = async {
        until("the slow sibling is busy", || {
            kit.ndjson.requests_for(SLOW_MODEL).len() == 1
        })
        .await;
        invoke(&tool, WORKER, "read the readme").await
    };
    let (slow, refused) = tokio::join!(slow, refused);

    assert!(
        matches!(refused, Err(InvokeAgentError::BudgetExhausted { .. })),
        "{refused:?}"
    );
    match slow {
        Err(InvokeAgentError::BudgetExhausted { ref agent, .. }) => assert_eq!(agent, SLOW),
        other => panic!("the running sibling is stopped by the budget: {other:?}"),
    }
    let participants = kit.participants();
    until("every worker is reaped", || {
        participants.open_runs() == 0
            && !participants
                .admitted()
                .iter()
                .any(|name| participants.is_registered(name))
    })
    .await;

    // The run starts nothing more.
    let admitted = participants.admitted().len();
    let again = invoke(&tool, WORKER, "try again").await;
    assert!(
        matches!(again, Err(InvokeAgentError::BudgetExhausted { .. })),
        "{again:?}"
    );
    assert_eq!(participants.admitted().len(), admitted, "no worker spawned");
    assert_eq!(kit.sse.requests_for(WORKER_MODEL).len(), 1);
    assert_eq!(kit.ndjson.requests_for(SLOW_MODEL).len(), 1);
}

/// A sub-leader whose worker came back budget-stopped writes its final
/// message without a model call — its endpoint would only refuse it — and
/// fails its own task with the stop, so the root stops too and shows that
/// message: the budget, and what was finished.
#[tokio::test]
async fn final_message_without_model_call_when_refused() {
    let kit = SwarmKit::start(
        vec![
            AgentDef::new(LEAD, LEAD_MODEL, Endpoint::Sse).sub_leader(),
            AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Ndjson),
        ],
        Script::new().route(
            LEAD_MODEL,
            [
                Reply::tool_call(
                    "invoke_agent",
                    serde_json::json!({ "agent": WORKER, "prompt": "read the readme" }),
                ),
                // What the endpoint answers the lead's next call, if it
                // makes one.
                cap_exceeded(),
            ],
        ),
        Script::new().route(
            WORKER_MODEL,
            [
                Reply::Say("The README's title is Chatty.".to_string()),
                read_readme(),
                cap_exceeded(),
            ],
        ),
    )
    .await;

    let run = kit.run_leader_to(LEAD, "summarise the readme").await;

    assert_eq!(
        kit.sse.requests_for(LEAD_MODEL).len(),
        1,
        "the lead makes no model call after the budget stop"
    );
    assert_eq!(kit.ndjson.requests_for(WORKER_MODEL).len(), 2);
    let error = run.output.as_ref().expect_err("the stop reaches the root");
    assert!(
        error.contains("budget_exhausted: Budget of $10.00 spent"),
        "{error}"
    );
    assert!(
        error.contains(
            "What 'kit-lead' finished:\nBudget of $10.00 spent; the model endpoint refused the \
             call (cap_exceeded). Here is what was finished:\n\n**kit-worker**\n\
             The README's title is Chatty."
        ),
        "the lead's final message lists what was finished: {error}"
    );
}
