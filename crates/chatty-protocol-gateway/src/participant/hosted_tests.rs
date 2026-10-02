//! The hosted broker (HS-4a, AGE-685): the root-broker core embedded over
//! connections the host accepted, with the hosted client as its root.
//!
//! Each worker here is the far end of a `UnixStream` pair the test made,
//! standing in for a vsock connection the host accepted once: the broker
//! binds nothing, so no gateway, Unix socket or TCP port is involved.

use std::time::Duration;

use chatty_fabric::wire::TaskIdentity;
use chatty_fabric::{
    ApprovalKind, ApprovalRequest, CallResult, InvokeAgentParams, Refusal, wire::AgentEntry,
};
use tokio::net::UnixStream;

use super::*;
use crate::participant::gate::BrokerRoot;
use crate::participant::{
    BrokerFrame, ParticipantCard, ParticipantConnection, ParticipantFrame, TaskState,
};

/// Generous for CI: only a failure waits this long.
const DEADLINE: Duration = Duration::from_secs(30);

/// A policy that lets the root call `analyst` and nothing else.
struct AnalystOnly;

impl CallPolicy for AnalystOnly {
    fn may_call(&self, _caller: &str, _callee: &str) -> Result<(), Refusal> {
        Ok(())
    }

    fn root_may_call(&self, callee: &str) -> Result<(), Refusal> {
        if callee == "analyst" {
            Ok(())
        } else {
            Err(Refusal::NotListed {
                caller: chatty_fabric::ROOT_NAME.to_string(),
                callee: callee.to_string(),
            })
        }
    }
}

/// Every decision record, in order; refuses every write once `failing`.
#[derive(Default)]
struct Recorded {
    records: Mutex<Vec<DecisionRecord>>,
    failing: std::sync::atomic::AtomicBool,
}

impl DecisionLog for Recorded {
    fn record(&self, record: &DecisionRecord) -> Result<(), String> {
        if self.failing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("the audit store is unreachable".to_string());
        }
        self.records.lock().unwrap().push(record.clone());
        Ok(())
    }
}

fn ada() -> HostedClient {
    HostedClient {
        tenant: "acme".to_string(),
        user: "ada".to_string(),
    }
}

fn mallory() -> HostedClient {
    HostedClient {
        tenant: "evil".to_string(),
        user: "mallory".to_string(),
    }
}

fn hosted_broker(root: BrokerRoot) -> (HostedBroker, Arc<Recorded>) {
    let log = Arc::new(Recorded::default());
    let config = HostedConfig {
        binding: Binding {
            tenant: "acme".to_string(),
            user: "ada".to_string(),
            root,
        },
        policy: Arc::new(AnalystOnly),
        log: log.clone(),
        edges: None,
    };
    let broker = HostedBroker::new(config, ParticipantRegistry::new(), BTreeMap::new());
    (broker, log)
}

/// What the scripted worker does with its one task.
#[derive(Clone, Copy)]
enum Script {
    /// Answer `the answer from <name>` at once.
    Answer,
    /// Ask the root for an approval first, and answer with its verdict.
    Approve,
}

/// Admit an `analyst`, serve its connection — one end of a stream pair,
/// as the host serves the vsock stream it accepted — and run a scripted
/// worker on the other end. Returns the admitted name and the identity
/// the worker's task carried, once it has one.
async fn analyst(
    broker: &HostedBroker,
    script: Script,
) -> (String, tokio::sync::oneshot::Receiver<Option<TaskIdentity>>) {
    let node = broker.admit("analyst", None).unwrap();
    let name = node.name().to_string();
    let (broker_end, worker_end) = UnixStream::pair().unwrap();
    tokio::spawn(broker.serve(broker_end, node));
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let mut connection =
            ParticipantConnection::hello_over(worker_end, ParticipantCard::default())
                .await
                .unwrap();
        let answer = format!("the answer from {}", connection.name());
        let mut seen = Some(seen_tx);
        while let Ok(Some(frame)) = connection.next_frame().await {
            let BrokerFrame::Task {
                task_id, identity, ..
            } = frame
            else {
                continue;
            };
            if let Some(seen) = seen.take() {
                let _ = seen.send(identity);
            }
            let text = match script {
                Script::Answer => answer.clone(),
                Script::Approve => {
                    connection
                        .send(ParticipantFrame::Approve {
                            id: 2,
                            request: ApprovalRequest {
                                kind: ApprovalKind::Exec,
                                command_or_path: "rm -rf build".to_string(),
                                diff_stat: None,
                                asker: None,
                            },
                        })
                        .await
                        .unwrap();
                    let verdict = loop {
                        match connection.next_frame().await {
                            Ok(Some(BrokerFrame::Approval { id: 2, verdict })) => break verdict,
                            Ok(Some(_)) => continue,
                            other => panic!("the approval was never answered: {other:?}"),
                        }
                    };
                    format!("{verdict:?}")
                }
            };
            connection.artifact(&task_id, text, true).await.unwrap();
            connection
                .finish(&task_id, TaskState::Completed, None, None)
                .await
                .unwrap();
        }
    });
    for _ in 0..1000 {
        if broker.registry().is_registered(&name) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(broker.registry().is_registered(&name), "{name} said hello");
    (name, seen_rx)
}

fn invoke(agent: &str) -> CallRequest {
    CallRequest::InvokeAgent(InvokeAgentParams {
        agent: agent.to_string(),
        prompt: "count the invoices".to_string(),
        handle: None,
        include_trace: false,
        spawn_context: None,
        remaining: Default::default(),
        run: None,
    })
}

/// Every event of `stream`, to its end.
async fn drain(stream: HostedStream) -> Vec<Result<HostedEvent, CallError>> {
    tokio::time::timeout(DEADLINE, stream.collect())
        .await
        .expect("the call ends")
}

fn response(events: &[Result<HostedEvent, CallError>]) -> String {
    match events.last() {
        Some(Ok(HostedEvent {
            event: CallEvent::Result(CallResult::Invoked(outcome)),
            ..
        })) => {
            assert!(outcome.success, "{outcome:?}");
            outcome.response.clone()
        }
        other => panic!("not an invoke result: {other:?}"),
    }
}

fn refusal(events: &[Result<HostedEvent, CallError>]) -> String {
    match events {
        [Err(error)] => error.to_string(),
        other => panic!("expected one refusal, got {other:?}"),
    }
}

/// A broker built from the embeddable core serves one hosted root
/// conversation: a worker on a connection the host accepted is welcomed
/// under the admitted name, the hosted client's run reaches it carrying the
/// binding's identity, and the answer comes back — with no gateway, socket
/// or TCP listener bound anywhere.
#[tokio::test]
async fn embedded_broker_serves_one_conversation() {
    let (broker, log) = hosted_broker(BrokerRoot::Hosted);
    let (name, identity) = analyst(&broker, Script::Answer).await;
    let root = broker.root();

    let listed = drain(root.call(&ada(), CallRequest::ListAgents)).await;
    let Some(Ok(HostedEvent {
        event: CallEvent::Result(CallResult::Agents(agents)),
        ..
    })) = listed.last()
    else {
        panic!("agent.list answers with the directory: {listed:?}");
    };
    assert!(
        agents.iter().any(|agent: &AgentEntry| agent.name == name),
        "{agents:?}"
    );

    let events = drain(root.call(&ada(), invoke(&name))).await;
    assert_eq!(response(&events), format!("the answer from {name}"));
    assert_eq!(
        identity.await.unwrap(),
        Some(TaskIdentity::new("acme", "ada")),
        "the task carried the broker's binding"
    );
    assert_eq!(root.conversation(&ada(), ConversationOp::Create), Ok(()));
    assert_eq!(root.read(&ada()), Ok(Vec::new()));

    let records = log.records.lock().unwrap().clone();
    let invoked = records
        .iter()
        .find(|record| record.row == "root/agent.invoke")
        .expect("the run's decision was written");
    assert_eq!(invoked.caller, "root:acme/ada");
    assert_eq!(invoked.refused, None);
    assert!(
        records
            .iter()
            .any(|record| record.row == "root/conversation.create"),
        "{records:?}"
    );
}

/// Another tenant's client is refused at the handle — start, cancel, read
/// and answer — and its refused start reaches no worker.
#[tokio::test]
async fn hosted_client_cross_tenant_refused() {
    let (broker, log) = hosted_broker(BrokerRoot::Hosted);
    let (name, _identity) = analyst(&broker, Script::Answer).await;
    let root = broker.root();

    let started = drain(root.call(&mallory(), invoke(&name))).await;
    assert!(
        refusal(&started).contains("another tenant or user"),
        "{started:?}"
    );
    assert_eq!(broker.registry().open_task_count(&name), 0);
    for refused in [
        root.cancel(&mallory(), &name),
        root.read(&mallory()).map(|_| ()),
        root.answer(&mallory(), "question-1", Some("guess"), Vec::new()),
        root.approve(
            &mallory(),
            "approval-1",
            Some("guess"),
            ApprovalVerdict::Approved,
        ),
        root.conversation(&mallory(), ConversationOp::Delete),
    ] {
        let error = refused.expect_err("another tenant is refused");
        assert!(
            error.to_string().contains("another tenant or user"),
            "{error}"
        );
    }
    // The same tenant as another user is another client.
    let eve = HostedClient {
        tenant: "acme".to_string(),
        user: "eve".to_string(),
    };
    assert!(root.read(&eve).is_err());
    assert!(
        log.records
            .lock()
            .unwrap()
            .iter()
            .filter(|record| record.caller == "root:evil/mallory")
            .all(|record| record.refused.is_some()),
        "every cross-tenant decision was written as a refusal"
    );
}

/// An approval's answer names its pending id and the nonce handed out with
/// it: none (session-only) or the wrong one is refused, and the run waits
/// until the right one answers.
#[tokio::test]
async fn hosted_answer_without_nonce_refused() {
    let (broker, _log) = hosted_broker(BrokerRoot::Hosted);
    let (name, _identity) = analyst(&broker, Script::Approve).await;
    let root = broker.root();

    let mut stream = root.call(&ada(), invoke(&name));
    let (id, nonce) = loop {
        let event = tokio::time::timeout(DEADLINE, stream.next())
            .await
            .expect("the approval arrives")
            .expect("the call is open")
            .expect("no error");
        if let CallEvent::Approve { id, .. } = &event.event {
            break (
                id.clone(),
                event.nonce.expect("an approval carries a nonce"),
            );
        }
    };

    let session_only = root
        .approve(&ada(), &id, None, ApprovalVerdict::Approved)
        .expect_err("a nonce-less answer is refused");
    assert!(
        session_only.to_string().contains("without its nonce"),
        "{session_only}"
    );
    let forged = root
        .approve(&ada(), &id, Some("0000"), ApprovalVerdict::Approved)
        .expect_err("another nonce is refused");
    assert!(
        forged.to_string().contains("no pending request"),
        "{forged}"
    );
    let nonce_less_question = root.answer(&ada(), "question-1", None, Vec::new());
    assert!(nonce_less_question.is_err());

    root.approve(&ada(), &id, Some(&nonce), ApprovalVerdict::Denied)
        .expect("the right nonce answers");
    let rest: Vec<_> = tokio::time::timeout(DEADLINE, stream.collect())
        .await
        .expect("the call ends");
    assert_eq!(response(&rest), "Denied");
    // Spent: the same nonce answers nothing twice.
    assert!(
        root.approve(&ada(), &id, Some(&nonce), ApprovalVerdict::Approved)
            .is_err()
    );
}

/// On a typed-root broker the hosted client only cancels and reads; it
/// never starts, lists, answers or keeps conversations.
#[tokio::test]
async fn hosted_client_on_typed_root_only_cancels_and_reads() {
    let (broker, _log) = hosted_broker(BrokerRoot::Typed { key_owner: None });
    let root = broker.root();

    assert_eq!(root.read(&ada()), Ok(Vec::new()));
    // Granted, then nothing runs under that name.
    let nothing = root.cancel(&ada(), "analyst-0").unwrap_err();
    assert!(nothing.to_string().contains("nothing running"), "{nothing}");
    for request in [invoke("analyst"), CallRequest::ListAgents] {
        let events = drain(root.call(&ada(), request)).await;
        assert!(refusal(&events).contains("typed-root"), "{events:?}");
    }
    assert!(
        root.approve(&ada(), "approval-1", Some("n"), ApprovalVerdict::Approved)
            .is_err()
    );
    assert!(
        root.conversation(&ada(), ConversationOp::List)
            .unwrap_err()
            .to_string()
            .contains("typed-root")
    );

    // An `External`-rooted broker also needs the key's owner.
    let (owned, _log) = hosted_broker(BrokerRoot::Typed {
        key_owner: Some("grace".to_string()),
    });
    assert!(owned.root().read(&ada()).is_err());
}

/// A decision the hosted broker cannot write is refused as `internal`
/// before its effect.
#[tokio::test]
async fn hosted_decision_log_failure_refuses() {
    let (broker, log) = hosted_broker(BrokerRoot::Hosted);
    let (name, _identity) = analyst(&broker, Script::Answer).await;
    log.failing.store(true, std::sync::atomic::Ordering::SeqCst);

    let events = drain(broker.root().call(&ada(), invoke(&name))).await;
    assert!(
        refusal(&events).contains("internal: the decision log write failed"),
        "{events:?}"
    );
    assert_eq!(broker.registry().open_task_count(&name), 0);
}
