//! AGE-646 (DP-5): an execution approval raised at depth 2 reaches the
//! root's approval store — and since EN-2a (AGE-770) only the root's: the
//! root answers it, not the intermediate.
//!
//! Every hop is the real thing: the root is a real agent in this process
//! whose shell asks before every command ([`KitRoot`]), `kit-lead` and
//! `kit-coder` are real `chatty-tui` workers that ask too (no
//! `--auto-approve`), and the broker between them is the kit's. The coder's
//! `shell_execute` sends `human.approve` on its connection; the broker
//! delivers it straight to the root's call, past the lead, and the root
//! raises it on its store, which is the human's card. The verdict goes
//! back to the coder's request the same way.

use chatty_core::models::execution_approval_store::ApprovalNotification;
use chatty_core::testing::fake_model::{RecordedRequest, Reply, Script};

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

/// root → kit-lead → kit-coder, every level asking before each command.
/// The coder runs `echo hi` and answers with what it saw.
async fn asking_chain() -> SwarmKit {
    SwarmKit::start_asking(
        vec![
            AgentDef::new(LEAD, LEAD_MODEL, Endpoint::Sse).sub_leader(),
            AgentDef::new(CODER, CODER_MODEL, Endpoint::Ndjson),
        ],
        Script::new()
            .route(
                ROOT_MODEL,
                [invoke(LEAD, "Run it."), Reply::text("Root done.")],
            )
            .route(
                LEAD_MODEL,
                [invoke(CODER, "Say hi."), Reply::text("Lead done.")],
            ),
        Script::new().route(
            CODER_MODEL,
            [
                Reply::tool_call("shell_execute", serde_json::json!({ "command": "echo hi" })),
                Reply::text("Coder done."),
            ],
        ),
    )
    .await
}

/// The last message of a request: the tool result the model reads next.
fn last_message(request: &RecordedRequest) -> String {
    request.json()["messages"]
        .as_array()
        .and_then(|messages| messages.last())
        .map(|message| message.to_string())
        .unwrap_or_default()
}

/// The coder's requests once the root's run has ended.
async fn finish(root_run: tokio::task::JoinHandle<String>, kit: &SwarmKit) -> Vec<RecordedRequest> {
    let answer = tokio::time::timeout(DEADLINE, root_run)
        .await
        .expect("the root's run ends before the deadline")
        .expect("the root's run does not panic");
    assert_eq!(answer, "Root done.");
    kit.ndjson.requests_for(CODER_MODEL)
}

/// The approval the root's store received, checked to be the coder's
/// command: it reached the root from depth 2.
async fn the_relayed_approval(root: &mut KitRoot) -> ApprovalNotification {
    let approval = root.next_approval().await;
    assert!(
        approval.command.ends_with("[shell] echo hi"),
        "the root's store holds the coder's command: {approval:?}"
    );
    assert_eq!(approval.detail.command_or_path, "[shell] echo hi");
    assert_eq!(
        root.approvals.pending_ids(),
        std::slice::from_ref(&approval.id)
    );
    approval
}

/// DP-5 invariant 9: a shell approval at depth 2 appears on the root's
/// approval store; approve → the command runs in the depth-2 worker; deny →
/// a tool error; root cancel → the parked request is cancelled.
#[tokio::test]
async fn approval_relays_up_the_chain() {
    // Approve: the command runs in the coder, whose model reads its output.
    {
        let kit = asking_chain().await;
        let mut root = KitRoot::build(&kit).await;
        let run = root.run("Start.");
        let approval = the_relayed_approval(&mut root).await;
        assert_eq!(
            kit.ndjson.requests_for(CODER_MODEL).len(),
            1,
            "the coder is parked on the approval, not past it"
        );
        root.approve(&approval.id);

        let coder = finish(run, &kit).await;
        assert_eq!(coder.len(), 2, "the coder's model is asked again");
        let result = last_message(&coder[1]);
        assert!(
            result.contains(r#"\"stdout\":\"hi\""#) && result.contains(r#"\"exit_code\":0"#),
            "the coder's next request carries the command's output: {result}"
        );
    }

    // Deny: the coder's tool fails, and its model reads the error.
    {
        let kit = asking_chain().await;
        let mut root = KitRoot::build(&kit).await;
        let run = root.run("Start.");
        let approval = the_relayed_approval(&mut root).await;
        root.deny(&approval.id);

        let coder = finish(run, &kit).await;
        assert_eq!(coder.len(), 2);
        let result = last_message(&coder[1]);
        assert!(
            result.contains("Execution denied by user"),
            "the coder's next request carries the tool error: {result}"
        );
    }

    // Root cancel: the root's re-raised request is withdrawn as its turn
    // goes, and the chain under it is torn down without the command running.
    {
        let kit = asking_chain().await;
        let mut root = KitRoot::build(&kit).await;
        let run = root.run("Start.");
        let approval = the_relayed_approval(&mut root).await;

        run.abort();
        assert!(run.await.unwrap_err().is_cancelled());
        // Ordered after the cancel, not timed: the request left the store
        // in the same step that dropped the root's turn.
        assert!(
            root.approvals.pending_ids().is_empty(),
            "the root's parked request is cancelled with its turn"
        );
        let withdrawn = root
            .resolved
            .try_recv()
            .expect("the root's card is told the request is over");
        assert_eq!(withdrawn.id, approval.id);
        assert!(!withdrawn.approved);

        // The broker drops the cancelled call, which reaps the lead, whose
        // call to the coder goes with it.
        tokio::time::timeout(DEADLINE, async {
            let participants = kit.participants();
            while participants.open_runs() > 0
                || participants
                    .admitted()
                    .iter()
                    .any(|name| participants.is_registered(name))
            {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("every worker under the cancelled root goes away");
        assert_eq!(
            kit.ndjson.requests_for(CODER_MODEL).len(),
            1,
            "the coder never ran the command it was waiting to run"
        );
    }
}

/// AGE-646 Do 4: the card the root's human sees names the agent that asked —
/// its broker-assigned name and the chain it runs under — not the lead that
/// relayed it.
#[tokio::test]
async fn approval_card_names_the_asking_agent() {
    let kit = asking_chain().await;
    let mut root = KitRoot::build(&kit).await;
    let run = root.run("Start.");
    let approval = the_relayed_approval(&mut root).await;

    // AGE-751: `command` is the command alone — the asker never folds into
    // it — so a card that wraps `command` in backticks never wraps the
    // asker along with it.
    assert_eq!(approval.command, "[shell] echo hi");
    let asker = approval.detail.asker.clone().expect("a relayed approval");
    assert_eq!(asker.agent, "kit-coder-0");
    assert_eq!(asker.chain, ["root", "kit-lead", "kit-coder"]);

    root.approve(&approval.id);
    finish(run, &kit).await;
}
