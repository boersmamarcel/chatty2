//! EN-2a (AGE-770): only the root answers approvals, end to end.
//!
//! The root is a real agent in this process ([`KitRoot`]); `kit-lead` and
//! `kit-coder` are real `chatty-tui` workers that ask before every command.
//! The coder's `human.approve` goes from its connection straight to the
//! root's call: the lead never sees it.

use chatty_core::settings::models::execution_settings::ApprovalMode;
use chatty_core::testing::fake_model::{Reply, Script};

use super::swarm_kit::{AgentDef, Endpoint, KitRoot, ROOT_MODEL, SwarmKit};

const LEAD: &str = "kit-lead";
const LEAD_MODEL: &str = "kit/lead";
const CODER: &str = "kit-coder";
const CODER_MODEL: &str = "kit/coder";

/// Generous for CI: only a failure waits this long.
const DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

fn invoke(agent: &str, prompt: &str) -> Reply {
    Reply::tool_call(
        "invoke_agent",
        serde_json::json!({ "agent": agent, "prompt": prompt }),
    )
}

fn shell(command: &str) -> Reply {
    Reply::tool_call("shell_execute", serde_json::json!({ "command": command }))
}

/// root → kit-lead → kit-coder; the coder runs `echo hi`. The root runs
/// `root_first` before it delegates.
async fn asking_chain(root_first: Vec<Reply>) -> SwarmKit {
    let mut root = root_first;
    root.extend([invoke(LEAD, "Run it."), Reply::text("Root done.")]);
    SwarmKit::start_asking(
        vec![
            AgentDef::new(LEAD, LEAD_MODEL, Endpoint::Sse).sub_leader(),
            AgentDef::new(CODER, CODER_MODEL, Endpoint::Ndjson),
        ],
        Script::new().route(ROOT_MODEL, root).route(
            LEAD_MODEL,
            [invoke(CODER, "Say hi."), Reply::text("Lead done.")],
        ),
        Script::new().route(CODER_MODEL, [shell("echo hi"), Reply::text("Coder done.")]),
    )
    .await
}

async fn finish(run: tokio::task::JoinHandle<String>) {
    let answer = tokio::time::timeout(DEADLINE, run)
        .await
        .expect("the root's run ends before the deadline")
        .expect("the root's run does not panic");
    assert_eq!(answer, "Root done.");
}

/// EN-2a: a forwarded approval always asks the human. The root runs under
/// `AutoApproveAll` — its own command goes through without a card — yet the
/// coder's command reaches the root's human as a card, not sandboxed, and
/// waits for them. No approval classifier exists to consult (ADR-0008's is
/// not built); the forwarded path asks the store directly, so neither it
/// nor the mode nor the callee's sandbox can decide.
#[tokio::test]
async fn forwarded_approval_ignores_root_auto_approve_and_classifier() {
    let kit = asking_chain(vec![shell("echo root")]).await;
    let mut root = KitRoot::build_with_mode(&kit, ApprovalMode::AutoApproveAll).await;
    let run = root.run("Start.");

    let card = root.next_approval().await;
    assert_eq!(
        card.command, "[shell] echo hi",
        "the first card is the coder's: the root's own command was auto-approved"
    );
    assert!(!card.is_sandboxed);
    assert_eq!(
        card.detail.asker.as_ref().map(|asker| asker.agent.as_str()),
        Some("kit-coder-0")
    );
    assert_eq!(
        kit.ndjson.requests_for(CODER_MODEL).len(),
        1,
        "the coder waits on the human"
    );

    root.deny(&card.id);
    finish(run).await;
    let coder = kit.ndjson.requests_for(CODER_MODEL);
    assert_eq!(coder.len(), 2);
    let result = coder[1].json()["messages"]
        .as_array()
        .and_then(|messages| messages.last())
        .map(|message| message.to_string())
        .unwrap_or_default();
    assert!(
        result.contains("Execution denied by user"),
        "the human's deny stands: {result}"
    );
}

/// EN-2a: when the callee that asked is stopped, its card leaves the
/// root's screen — withdrawn under the broker's id — while the root's run
/// carries on to its end.
#[tokio::test]
async fn callee_end_withdraws_root_approval_card() {
    let kit = asking_chain(Vec::new()).await;
    let mut root = KitRoot::build(&kit).await;
    let run = root.run("Start.");

    let card = root.next_approval().await;
    assert_eq!(card.command, "[shell] echo hi");
    kit.broker()
        .transport()
        .cancel("kit-coder-0")
        .expect("the coder is stopped");

    let withdrawn = tokio::time::timeout(DEADLINE, root.resolved.recv())
        .await
        .expect("the card is withdrawn before the deadline")
        .expect("the resolution channel is open");
    assert_eq!(withdrawn.id, card.id);
    assert!(!withdrawn.approved);
    assert!(root.approvals.pending_ids().is_empty());

    finish(run).await;
    assert_eq!(
        kit.ndjson.requests_for(CODER_MODEL).len(),
        1,
        "the coder never ran the command it was waiting to run"
    );
}
