//! AGE-306's verification: a two-level chain — parent → child → grandchild
//! — surfaces the grandchild's question in the parent's popover and
//! delivers the answer back down.
//!
//! Every hop is the real thing: a `ProtocolGateway` on a real port with a
//! real participant socket, the real one-task worker loop over that socket
//! at both worker levels, and the real `invoke_agent` tool at both caller
//! levels. Only the LLM turn is a closure, because that is the one thing
//! the chain must not depend on: the grandchild's turn calls `ask_user`'s
//! own `request_clarification`, the child's turn calls `invoke_agent`, and
//! the parent is the test itself, playing the human behind the popover.
//!
//! What makes it a *chain* rather than a hop: the child has no human. Its
//! `invoke_agent` re-asks the grandchild's question on the child's own
//! clarification store, which parks the child's task in `input-required`
//! toward the parent — the same path, one level up.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use chatty_core::models::clarification_store::{
    ClarificationAnswer, ClarificationStore, ClarifyingQuestion, request_clarification,
};
use chatty_core::services::install_progress_channel;
use chatty_core::session::SessionEvent;
use chatty_core::tools::invoke_agent_tool::{
    InvokeAgentArgs, InvokeAgentProgress, InvokeAgentTool,
};
use chatty_module_registry::ModuleRegistry;
use chatty_protocol_gateway::ProtocolGateway;
use chatty_protocol_gateway::participant::ParticipantRegistry;
use chatty_protocol_gateway::worker::{
    EventSink, InputReceiver, answer_clarifications, serve_one_task, worker_card,
};
use chatty_wasm_runtime::{CompletionResponse, LlmProvider, Message, ResourceLimits};
use rig_agent::tool::{Tool, ToolContext};
use tokio::net::UnixStream;
use tokio::sync::{RwLock, mpsc};

const GRANDCHILD: &str = "grandchild";
const CHILD: &str = "child";

/// Generous for CI; the chain settles in milliseconds.
const DEADLINE: Duration = Duration::from_secs(20);

struct NoopProvider;

impl LlmProvider for NoopProvider {
    fn complete(
        &self,
        _model: &str,
        _messages: Vec<Message>,
        _tools: Option<String>,
    ) -> Result<CompletionResponse, String> {
        Err("noop".into())
    }
}

/// A gateway on an ephemeral port with a participant socket in a temp dir.
struct Broker {
    port: u16,
    socket: std::path::PathBuf,
    participants: ParticipantRegistry,
    _dir: tempfile::TempDir,
}

impl Broker {
    async fn start() -> Self {
        let dir = tempfile::tempdir().expect("a temp dir for the socket");
        let socket = dir.path().join("participants.sock");

        let provider: Arc<dyn LlmProvider> = Arc::new(NoopProvider);
        let modules = Arc::new(RwLock::new(
            ModuleRegistry::new(provider, ResourceLimits::default()).unwrap(),
        ));
        let gateway = ProtocolGateway::new(modules, 0).with_participant_socket(&socket);
        let participants = gateway.participants();

        let listener =
            chatty_protocol_gateway::participant::bind(&socket).expect("the socket binds");
        tokio::spawn(chatty_protocol_gateway::participant::serve(
            listener,
            participants.clone(),
        ));

        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = tcp.local_addr().unwrap().port();
        let router = gateway.build_router();
        tokio::spawn(async move {
            axum::serve(tcp, router).await.ok();
        });

        Self {
            port,
            socket,
            participants,
            _dir: dir,
        }
    }

    /// The `invoke_agent` a level of the chain holds, addressing `agent`
    /// through this broker and re-asking its questions on `store`.
    fn invoke_agent(&self, agent: &str, store: Option<&ClarificationStore>) -> InvokeAgentTool {
        let tool = InvokeAgentTool::new(vec![], vec![], Some(self.port)).with_local_agents([agent]);
        match store {
            Some(store) => tool.with_clarifications(store.get_pending_clarifications()),
            None => tool,
        }
    }

    async fn await_registration(&self, name: &str) {
        for _ in 0..200 {
            if self.participants.is_registered(name) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("participant '{name}' never registered");
    }
}

/// The part of a session a scripted turn needs: a clarification store whose
/// requests become `ClarificationRequested` events on the turn's sink —
/// which is what `AgentSession`'s stream loop does for a real turn — and
/// whose answers arrive from the broker.
fn scripted_session(sink: &EventSink, inputs: InputReceiver) -> ClarificationStore {
    let mut store = ClarificationStore::new();
    let (notify_tx, mut notify_rx) = mpsc::unbounded_channel();
    store.set_notifier(notify_tx);
    tokio::spawn({
        let sink = sink.clone();
        async move {
            while let Some(request) = notify_rx.recv().await {
                sink(&SessionEvent::ClarificationRequested {
                    id: request.id,
                    questions: request.questions,
                });
            }
        }
    });
    tokio::spawn(answer_clarifications(inputs, store.clone()));
    store
}

fn tool_started(sink: &EventSink, name: &str) {
    sink(&SessionEvent::ToolCallStarted {
        id: format!("{name}-1"),
        name: name.to_string(),
    });
}

fn tool_finished(sink: &EventSink, name: &str, result: &str) {
    sink(&SessionEvent::ToolCallResult {
        id: format!("{name}-1"),
        result: result.to_string(),
    });
}

fn the_question() -> ClarifyingQuestion {
    ClarifyingQuestion {
        id: "q1".to_string(),
        question: "Which database?".to_string(),
        options: vec!["Postgres".to_string(), "SQLite".to_string()],
    }
}

/// The grandchild: a worker whose turn asks the user one question and
/// answers with whatever it is told.
async fn grandchild(broker: &Broker) {
    let stream = UnixStream::connect(&broker.socket).await.unwrap();
    let card = worker_card(GRANDCHILD, "test");
    tokio::spawn(serve_one_task(
        stream,
        card,
        |_task, sink, inputs| async move {
            let store = scripted_session(&sink, inputs);
            sink(&SessionEvent::TurnStarted);
            tool_started(&sink, "ask_user");
            let answers =
                request_clarification(&store.get_pending_clarifications(), vec![the_question()])
                    .await?;
            tool_finished(&sink, "ask_user", "answered");
            let choice = answers
                .first()
                .map(|a| a.answer.clone())
                .ok_or_else(|| anyhow!("no answer came back"))?;
            sink(&SessionEvent::Text(format!("Using {choice}.")));
            Ok(())
        },
    ));
    broker.await_registration(GRANDCHILD).await;
}

/// The child: a worker with no human, whose turn delegates to the
/// grandchild and repeats its answer.
async fn child(broker: &Broker) {
    let stream = UnixStream::connect(&broker.socket).await.unwrap();
    let card = worker_card(CHILD, "test");
    let port = broker.port;
    tokio::spawn(serve_one_task(
        stream,
        card,
        move |task, sink, inputs| async move {
            let store = scripted_session(&sink, inputs);
            let tool = InvokeAgentTool::new(vec![], vec![], Some(port))
                .with_local_agents([GRANDCHILD])
                .with_clarifications(store.get_pending_clarifications());
            sink(&SessionEvent::TurnStarted);
            tool_started(&sink, "invoke_agent");
            let out = tool
                .call(
                    &mut ToolContext::new(),
                    InvokeAgentArgs {
                        agent: GRANDCHILD.to_string(),
                        prompt: task.text,
                    },
                )
                .await
                .map_err(|e| anyhow!("{e}"))?;
            tool_finished(&sink, "invoke_agent", &out.response);
            sink(&SessionEvent::Text(out.response));
            Ok(())
        },
    ));
    broker.await_registration(CHILD).await;
}

/// The issue's "Verify": parent → child → grandchild, the grandchild's
/// question in the parent's popover, the answer back down the chain.
#[tokio::test]
async fn a_grandchilds_question_reaches_the_parents_popover_and_its_answer_comes_back() {
    let broker = Broker::start().await;
    grandchild(&broker).await;
    child(&broker).await;

    // The parent: the level facing a human. Its store's notifier is the
    // popover — that is exactly what a frontend listens on.
    let mut parent_store = ClarificationStore::new();
    let (popover_tx, mut popover) = mpsc::unbounded_channel();
    parent_store.set_notifier(popover_tx);
    let tool = broker.invoke_agent(CHILD, Some(&parent_store));
    let mut progress_rx = install_progress_channel(&tool.progress_slot());

    let delegation = tokio::spawn(async move {
        tool.call(
            &mut ToolContext::new(),
            InvokeAgentArgs {
                agent: CHILD.to_string(),
                prompt: "Set up the database.".to_string(),
            },
        )
        .await
    });

    // The grandchild's question, two levels down, arrives in the parent's
    // popover intact: text, id and options.
    let asked = tokio::time::timeout(DEADLINE, popover.recv())
        .await
        .expect("the question reaches the parent before the deadline")
        .expect("the parent's store announces it");
    assert_eq!(asked.questions, vec![the_question()]);

    // The human answers.
    assert!(
        parent_store.resolve(
            &asked.id,
            vec![ClarificationAnswer {
                id: "q1".to_string(),
                answer: "SQLite".to_string(),
                custom: false,
            }]
        ),
        "the popover's answer lands on the request invoke_agent parked"
    );

    // ...and the answer goes back down: child → grandchild, whose reply
    // climbs back up through both delegations.
    let out = tokio::time::timeout(DEADLINE, delegation)
        .await
        .expect("the chain finishes before the deadline")
        .expect("the delegation task did not panic")
        .expect("the delegation succeeded");
    assert!(out.success);
    assert_eq!(out.response, "Using SQLite.");

    // The parent's transcript shows the child's tool activity, as for any
    // delegation; the question itself is the popover's, not a progress line.
    let mut progress = Vec::new();
    while let Ok(event) = progress_rx.try_recv() {
        if let InvokeAgentProgress::Text(text) = event {
            progress.push(text);
        }
    }
    assert!(
        progress.contains(&"invoke_agent".to_string())
            && progress.contains(&"\u{2713} invoke_agent".to_string()),
        "the parent sees the child's delegation start and finish: {progress:?}"
    );

    // Nothing is left parked: the tasks closed with their terminal status.
    assert_eq!(broker.participants.open_task_count(CHILD), 0);
    assert_eq!(broker.participants.open_task_count(GRANDCHILD), 0);
}

/// Escalate-to-human is the only policy (the issue's "Policy"): a level
/// with nobody to ask does not guess, it ends the delegation and says why.
#[tokio::test]
async fn a_question_nobody_can_answer_ends_the_delegation() {
    let broker = Broker::start().await;
    grandchild(&broker).await;

    let tool = broker.invoke_agent(GRANDCHILD, None);
    let err = tokio::time::timeout(
        DEADLINE,
        tool.call(
            &mut ToolContext::new(),
            InvokeAgentArgs {
                agent: GRANDCHILD.to_string(),
                prompt: "Set up the database.".to_string(),
            },
        ),
    )
    .await
    .expect("the delegation ends before the deadline")
    .expect_err("nobody can answer, so the delegation fails");

    let message = err.to_string();
    assert!(
        message.contains("nobody here can answer") && message.contains("Which database?"),
        "the model is told what was asked and that it went unanswered: {message}"
    );
}
