//! TB-6 (AGE-748): the running-agents overview, end to end.
//!
//! The root is a real agent in this process ([`KitRoot`]); `kit-lead` and
//! `kit-coder` are real `chatty-tui` workers, and the coder asks before
//! its command. What the root's `invoke_agent` reports is folded into a
//! [`SwarmTrace`] exactly as a frontend folds it, and the overview's rows
//! are read off that.

use chatty_core::services::running_agents::{Activity, RunningAgent, rows_of};
use chatty_core::services::swarm_trace::SwarmTrace;
use chatty_core::session::SessionEvent;
use chatty_core::tools::invoke_agent_tool::{InvokeAgentProgress, WaitingOn};
use tokio::sync::mpsc::UnboundedReceiver;

use super::root_only_approvals::{DEADLINE, asking_chain, finish};
use super::swarm_kit::KitRoot;

/// The progress event as the session emits it.
fn session_event(progress: InvokeAgentProgress) -> SessionEvent {
    match progress {
        InvokeAgentProgress::Swarm(batch) => SessionEvent::SwarmEvent(batch),
        progress => SessionEvent::Delegation(progress),
    }
}

/// Fold progress into `trace` until the conversation's rows satisfy
/// `done`; those rows.
async fn rows_until(
    progress: &mut UnboundedReceiver<InvokeAgentProgress>,
    trace: &mut SwarmTrace,
    done: impl Fn(&[RunningAgent]) -> bool,
) -> Vec<RunningAgent> {
    tokio::time::timeout(DEADLINE, async {
        loop {
            let rows = rows_of("conv", "Start", trace);
            if done(&rows) {
                return rows;
            }
            let event = progress.recv().await.expect("the progress channel is open");
            trace.apply(&session_event(event));
        }
    })
    .await
    .expect("the overview reaches the expected rows before the deadline")
}

fn row<'a>(rows: &'a [RunningAgent], agent: &str) -> Option<&'a RunningAgent> {
    rows.iter().find(|row| row.agent == agent)
}

/// TB-6: while the coder waits on the root's human for its approval, the
/// overview shows it as waiting on approval under its lead — the asker
/// is the broker's stamp — and its lead as running; once the human
/// answers, it runs again, and when the turn ends nothing is live.
#[tokio::test]
async fn waiting_on_approval_is_shown() {
    let kit = asking_chain(Vec::new()).await;
    let mut root = KitRoot::build(&kit).await;
    let mut progress = root.watch_progress();
    let mut trace = SwarmTrace::new();
    trace.apply(&SessionEvent::TurnStarted);
    let run = root.run("Start.");

    let card = root.next_approval().await;
    let rows = rows_until(&mut progress, &mut trace, |rows| {
        row(rows, "kit-coder-0").is_some_and(|coder| coder.activity.is_waiting())
    })
    .await;
    let coder = row(&rows, "kit-coder-0").unwrap();
    assert_eq!(coder.activity, Activity::Waiting(WaitingOn::Approval));
    assert_eq!(coder.activity.label(), "waiting on approval");
    assert_eq!(coder.chain, ["root", "kit-lead-0"]);
    assert_eq!(coder.conversation, "Start");
    let lead = row(&rows, "kit-lead-0").expect("the lead is live");
    assert_eq!(lead.activity, Activity::Running);
    assert_eq!(lead.chain, ["root"]);

    root.approve(&card.id);
    rows_until(&mut progress, &mut trace, |rows| {
        row(rows, "kit-coder-0").is_none_or(|coder| coder.activity == Activity::Running)
    })
    .await;

    finish(run).await;
    while let Ok(event) = progress.try_recv() {
        trace.apply(&session_event(event));
    }
    trace.apply(&SessionEvent::TurnEnded);
    assert!(rows_of("conv", "Start", &trace).is_empty());
}
