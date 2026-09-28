//! Say hello, run one delegated task, report, return.
//!
//! This is `chatty-tui`'s AGE-301 participant loop with the agent lifted out
//! of it. What is left is everything the wire contract owns — the
//! `hello`/`welcome` exchange, the frame ordering, the single terminal status — and a hole
//! where the turn goes. The desktop fills the hole with `run_headless`; a
//! microVM's `chatty-server` fills it with `AgentSession::begin_turn`
//! (AGE-307). Neither of them gets to decide what the parent sees.
//!
//! # One task per process
//!
//! The loop reads frames until it gets a task, runs it, sends one terminal
//! status and returns. Every runner in the broker spawns a worker per task
//! and reaps it, so a second task would never arrive; keeping the lifecycle
//! this simple is what makes ADR-0011's second kill criterion (AGE-302) a
//! comparison of the hop rather than of process-reuse strategies.
//!
//! A consequence: a `cancel` frame that arrives mid-task is not acted on
//! here — cancellation is enforced by the broker reaping the worker, which
//! for a microVM means killing it. When workers become persistent, this is
//! the loop that grows a cancel path.
//!
//! # The way back down
//!
//! The socket is read for the whole task, not only until the task arrives:
//! a worker whose `ask_user` parked the task gets its answer as an `input`
//! frame (AGE-306), which lands on the [`InputReceiver`] the turn was
//! handed. What the turn does with it is [`answer_clarifications`] — the
//! embedder spawns that beside its turn with the session's store.
//!
//! # Calls (BI-4)
//!
//! The same connection carries the worker's own calls: its `invoke_agent`
//! and `list_agents` go up as `call` frames and their replies come down
//! beside the task's answers. A callee's question comes down as
//! `call_input_required` and the worker's answer goes up as `call_input`
//! (BI-5), so a question from any depth reaches the root's human.
//! [`WorkerConnection`] is a welcomed connection with a [`SocketTransport`]
//! over it, so the worker connects first and
//! builds its agent second, handing the agent [`WorkerConnection::transport`]
//! (connect-then-build). The read half routes each `call_*` reply to the
//! call it names; the write half interleaves `call` frames with the task's
//! progress.

use std::future::Future;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use chatty_core::services::fabric_transport::{CallReplies, Outbound, SocketTransport};
use chatty_core::services::{StreamError, StreamErrorKind};
use chatty_core::session::SessionEvent;
use chatty_fabric::Transport;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::{InputReceiver, TaskMapper};
use crate::participant::{
    BrokerFrame, DelegatedTask, ParticipantCard, ParticipantConnection, ParticipantFrame,
    ParticipantReader, ParticipantSkill,
};

#[cfg(doc)]
use super::answer_clarifications;

/// Where a running turn's events go. The same shape `chatty-tui`'s headless
/// runner already takes as an observer, so the desktop hands it straight on.
pub type EventSink = Arc<dyn Fn(&SessionEvent) + Send + Sync>;

/// What a worker publishes about itself.
///
/// Deliberately thin. A worker's skills are its tool set, which depends on
/// settings the broker already knows, and re-advertising them here would let
/// the two disagree. Live discovery is AGE-304's. `version` is the worker
/// binary's, so a broker log says which build answered. There is no name:
/// the broker assigns it with the connection (ADR-0020), so the card leaves
/// it empty.
pub fn worker_card(version: &str) -> ParticipantCard {
    ParticipantCard {
        name: String::new(),
        display_name: Some("chatty worker".to_string()),
        description: "A chatty agent delegated a task by the broker.".to_string(),
        version: version.to_string(),
        skills: vec![ParticipantSkill {
            name: "delegate".to_string(),
            description: "Run a self-contained task and report the result.".to_string(),
            examples: Vec::new(),
        }],
    }
}

/// A welcomed connection to the broker, with the transport the worker's
/// calls travel over. Connect, build the agent with
/// [`transport`](Self::transport), then [`serve_one_task`](Self::serve_one_task).
pub struct WorkerConnection {
    connection: ParticipantConnection,
    transport: Arc<SocketTransport>,
    calls: mpsc::UnboundedReceiver<Outbound>,
    replies: CallReplies,
}

impl WorkerConnection {
    /// Say hello over `stream`, the connection the broker made for this
    /// worker, and wait for the `welcome`.
    ///
    /// `stream` is the worker's end of a socket pair on the desktop, a vsock
    /// stream from inside a microVM (AGE-307, HS-4). Which one it is changes
    /// nothing below this line, which is the property C8 is built on.
    pub async fn connect<S>(stream: S, card: ParticipantCard) -> Result<Self>
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let connection = ParticipantConnection::hello_over(stream, card)
            .await
            .context("the broker did not welcome this worker")?;
        info!(participant = connection.name(), "Welcomed by the broker");
        let (transport, calls, replies) = SocketTransport::new();
        Ok(Self {
            connection,
            transport,
            calls,
            replies,
        })
    }

    /// The name the broker gave this worker.
    pub fn name(&self) -> &str {
        self.connection.name()
    }

    /// The name this worker's owner answers to, as the `welcome` gave it:
    /// the node that asked for this worker, or [`chatty_fabric::ROOT_NAME`]
    /// when the root did. Its `send_message` names it (tree messages).
    pub fn owner(&self) -> String {
        self.connection
            .owner()
            .map(|owner| owner.to_string())
            .unwrap_or_else(|| chatty_fabric::ROOT_NAME.to_string())
    }

    /// What this worker's `invoke_agent` and `list_agents` reach local
    /// roles and the broker's directory through (ADR-0020, BI-4).
    pub fn transport(&self) -> Arc<dyn Transport> {
        self.transport.clone()
    }

    /// Run the first task that arrives through `run`, and report its
    /// outcome.
    ///
    /// `run` is handed the task — its prompt and, when the caller presented
    /// one, their bearer (AGE-371) — the sink its turn's events must go to,
    /// and the receiver the broker's answers to the turn's questions arrive
    /// on; its `Err` becomes the task's failure message. It must not outlive
    /// the sink it was given — the sink is what closes the frame queue, and
    /// a copy left alive in a detached task would hold the terminal status
    /// behind it. Dropping the future that owns it, which is what awaiting
    /// `run` does, is enough.
    pub async fn serve_one_task<F, Fut>(self, run: F) -> Result<()>
    where
        F: FnOnce(DelegatedTask, EventSink, InputReceiver) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        let Self {
            mut connection,
            transport: _,
            mut calls,
            replies,
        } = self;

        let (task_id, task) = match next_task(&mut connection, &replies).await? {
            Some(task) => task,
            None => {
                debug!("The broker closed the socket before sending a task");
                return Ok(());
            }
        };
        info!(task = %task_id, "Received a delegated task");

        // Frames are produced on the turn's sink, which is synchronous, and
        // written by this task, which is not. The queue is the seam; it is
        // unbounded because dropping a progress frame would silently make the
        // parent's transcript wrong, which is the thing being measured.
        let (frames_tx, mut frames_rx) = mpsc::unbounded_channel::<ParticipantFrame>();
        let mapper = Arc::new(Mutex::new(
            TaskMapper::new(task_id.clone())
                .with_capture_conversation(task.capture_conversation)
                .with_handoff(task.handoff.clone())
                .with_swarm_events(task.swarm_events),
        ));

        let sink: EventSink = {
            let mapper = mapper.clone();
            Arc::new(move |event: &SessionEvent| {
                let mut mapper = lock(&mapper);
                if let Some(frame) = mapper.map(event) {
                    let _ = frames_tx.send(frame);
                }
                if let Some(frame) = mapper.swarm_event(event) {
                    let _ = frames_tx.send(frame);
                }
            })
        };

        let (reader, mut writer_half) = connection.into_split();
        // The turn's progress and its calls share the write half, until the
        // turn is over; then whatever progress is still queued goes out, and
        // the socket is free for the terminal status.
        let (turn_over, mut turn_over_rx) = tokio::sync::oneshot::channel::<()>();
        let writer = tokio::spawn(async move {
            let mut progress_open = true;
            loop {
                let frame = tokio::select! {
                    biased;
                    frame = frames_rx.recv(), if progress_open => match frame {
                        Some(frame) => frame,
                        None => {
                            progress_open = false;
                            continue;
                        }
                    },
                    Some(outbound) = calls.recv() => match outbound {
                        Outbound::Call { id, request } => ParticipantFrame::Call { id, request },
                        Outbound::Answer { id, task, input } => {
                            match serde_json::from_value(input) {
                                Ok(input) => ParticipantFrame::CallInput { id, task, input },
                                Err(e) => {
                                    warn!(call = id, error = %e, "Dropping an answer that is not an input frame's shape");
                                    continue;
                                }
                            }
                        }
                    },
                    _ = &mut turn_over_rx => break,
                };
                if let Err(e) = writer_half.send(frame).await {
                    warn!(error = %e, "Failed to write to the broker");
                    return Err(e);
                }
            }
            // The sink died with the turn's future, so this ends.
            while let Some(frame) = frames_rx.recv().await {
                if let Err(e) = writer_half.send(frame).await {
                    warn!(error = %e, "Failed to report progress to the broker");
                    return Err(e);
                }
            }
            Ok(writer_half)
        });

        // Answers and call replies come down the read half while the turn
        // runs. The reader lives exactly as long as the turn: the receiver it
        // feeds is dropped with the turn, and nothing after that can be
        // waiting for a frame.
        let (inputs_tx, inputs_rx) = mpsc::unbounded_channel();
        let reader = tokio::spawn(forward_inputs(
            reader,
            task_id.clone(),
            inputs_tx,
            replies.clone(),
        ));

        let outcome = run(task, sink, inputs_rx).await;
        reader.abort();
        replies.disconnected();
        let _ = turn_over.send(());

        // The sink died with the future that owned it, which closed the
        // queue and ended the writer; only then is the socket free for the
        // terminal status.
        let mut connection = writer
            .await
            .context("the frame writer panicked")?
            .context("the broker connection failed mid-task")?;

        let terminal = {
            let mut mapper = lock(&mapper);
            if let Err(e) = &outcome {
                // A failure `run` returns never reached the session as a
                // `SessionEvent`, so the mapper has not seen it.
                mapper.map(&SessionEvent::Error(StreamError {
                    kind: StreamErrorKind::Other,
                    message: format!("{e:#}"),
                }));
            }
            mapper.terminal()
        };

        info!(task = %task_id, "Reporting the task as finished");
        connection
            .send(terminal)
            .await
            .context("failed to report the task's final status")?;

        outcome
    }
}

/// Say hello over `stream`, run the first task that arrives through `run`,
/// and report its outcome: [`WorkerConnection::connect`] then
/// [`WorkerConnection::serve_one_task`], for a worker whose agent needs no
/// transport.
pub async fn serve_one_task<S, F, Fut>(stream: S, card: ParticipantCard, run: F) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Send + 'static,
    F: FnOnce(DelegatedTask, EventSink, InputReceiver) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    WorkerConnection::connect(stream, card)
        .await?
        .serve_one_task(run)
        .await
}

/// Hand a `call_*` frame to the call it names. `false` for any other frame.
fn route_reply(replies: &CallReplies, frame: &BrokerFrame) -> bool {
    match frame {
        BrokerFrame::CallProgress { id, event } => replies.progress(*id, event.clone()),
        BrokerFrame::CallResult { id, result } => replies.result(*id, result.clone()),
        BrokerFrame::CallError { id, error } => replies.error(*id, error.clone()),
        BrokerFrame::CallInputRequired { id, task, request } => {
            replies.input_required(*id, task.clone(), request.clone())
        }
        _ => return false,
    }
    true
}

/// Read frames for the running task until the broker closes the socket or
/// nobody is left to take an answer.
async fn forward_inputs(
    mut reader: ParticipantReader,
    task_id: String,
    inputs: mpsc::UnboundedSender<crate::participant::TaskInput>,
    replies: CallReplies,
) {
    loop {
        match reader.next_frame().await {
            Ok(Some(frame)) if route_reply(&replies, &frame) => {}
            Ok(Some(BrokerFrame::Input {
                task_id: for_task,
                input,
            })) if for_task == task_id => {
                if inputs.send(input).is_err() {
                    debug!(task = %task_id, "An answer arrived after the turn stopped listening");
                    break;
                }
            }
            Ok(Some(BrokerFrame::Input { task_id: other, .. })) => {
                debug!(task = %other, "Ignoring an answer for a task this worker does not run")
            }
            Ok(Some(BrokerFrame::Cancel { .. })) => {
                debug!(task = %task_id, "Cancel received; the broker reaps the worker")
            }
            Ok(Some(other)) => debug!(?other, "Ignoring an unexpected frame mid-task"),
            Ok(None) => {
                debug!(task = %task_id, "The broker closed the socket mid-task");
                break;
            }
            Err(e) => {
                warn!(task = %task_id, error = %e, "Lost the broker mid-task");
                break;
            }
        }
    }
    // Nothing more will come down: every call still waiting fails now
    // rather than hanging the turn.
    replies.disconnected();
}

/// Read frames until a task arrives, or the broker closes the socket.
async fn next_task(
    connection: &mut ParticipantConnection,
    replies: &CallReplies,
) -> Result<Option<(String, DelegatedTask)>> {
    while let Some(frame) = connection.next_frame().await? {
        if route_reply(replies, &frame) {
            continue;
        }
        match frame {
            BrokerFrame::Task {
                task_id,
                text,
                bearer,
                capture_conversation,
                spawn_context,
                handoff,
                swarm_events,
            } => {
                return Ok(Some((
                    task_id,
                    DelegatedTask::new(text)
                        .with_bearer(bearer)
                        .with_capture_conversation(capture_conversation)
                        .with_spawn_context(spawn_context)
                        .with_handoff(handoff)
                        .with_swarm_events(swarm_events),
                )));
            }
            BrokerFrame::Cancel { task_id } => {
                debug!(task = %task_id, "Ignoring a cancel for a task that never started")
            }
            other => debug!(?other, "Ignoring an unexpected frame before the first task"),
        }
    }
    Ok(None)
}

/// A poisoned mapper means a panic while a frame was being built. The mapper
/// is plain owned data with no cross-field invariant, so recovering it lets
/// the task still report a terminal status instead of hanging the parent on a
/// worker that will never answer.
fn lock(mapper: &Mutex<TaskMapper>) -> std::sync::MutexGuard<'_, TaskMapper> {
    mapper.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use chatty_fabric::{
        CallError, CallEvent, CallRequest, InvokeAgentOutcome, InvokeAgentParams, Transport,
    };
    use futures::StreamExt;
    use tokio::net::UnixStream;

    use super::*;
    use crate::participant::{
        BrokerCalls, LocalConnection, ParticipantRegistry, TaskState, open_connection,
    };

    /// A callee on its own broker-made connection that answers its one task
    /// with `answer from <its name>` after `delay`.
    async fn callee(registry: &ParticipantRegistry, spec: &str, delay: Duration) -> String {
        let LocalConnection { name, worker_end } = open_connection(registry, spec, None).unwrap();
        worker_end.set_nonblocking(true).unwrap();
        let stream = UnixStream::from_std(worker_end).unwrap();
        let mut connection = ParticipantConnection::hello_over(stream, worker_card("test"))
            .await
            .unwrap();
        let answer = format!("answer from {name}");
        tokio::spawn(async move {
            while let Ok(Some(frame)) = connection.next_frame().await {
                if let BrokerFrame::Task { task_id, .. } = frame {
                    tokio::time::sleep(delay).await;
                    connection
                        .artifact(&task_id, answer.clone(), true)
                        .await
                        .unwrap();
                    connection
                        .finish(&task_id, TaskState::Completed, None, None)
                        .await
                        .unwrap();
                    // Stay connected: the caller is not done with the rest.
                    std::future::pending::<()>().await;
                }
            }
        });
        name
    }

    fn invoke(agent: &str) -> CallRequest {
        CallRequest::InvokeAgent(InvokeAgentParams {
            agent: agent.to_string(),
            prompt: "go".to_string(),
            handle: None,
            include_trace: false,
            spawn_context: None,
        })
    }

    /// One call to the end: its result, or its error.
    async fn run_call(
        transport: &dyn Transport,
        agent: &str,
    ) -> Result<InvokeAgentOutcome, CallError> {
        let mut stream = transport.call(invoke(agent)).await?;
        while let Some(event) = stream.next().await {
            match event? {
                CallEvent::Result(result) => return Ok(serde_json::from_value(result).unwrap()),
                CallEvent::Progress(_) | CallEvent::InputRequired { .. } | CallEvent::Swarm(_) => {}
            }
        }
        panic!("call to {agent} ended without a result");
    }

    /// BI-4: three calls in flight in one task, over one connection, finish
    /// out of order, and each result reaches the call that asked for it. A
    /// fourth, to nobody, fails on its own without disturbing the rest.
    #[tokio::test]
    async fn concurrent_calls_match_by_id() {
        let registry = ParticipantRegistry::new();
        let calls = Arc::new(BrokerCalls::new(
            registry.clone(),
            Arc::new(BTreeMap::new()),
            None,
        ));
        registry.install_calls(&calls);

        // Issued in this order, finishing fast, mid, slow.
        let slow = callee(&registry, "slow", Duration::from_millis(600)).await;
        let fast = callee(&registry, "fast", Duration::from_millis(20)).await;
        let mid = callee(&registry, "mid", Duration::from_millis(300)).await;

        let LocalConnection { name, worker_end } =
            open_connection(&registry, "caller", None).unwrap();
        worker_end.set_nonblocking(true).unwrap();
        let worker = WorkerConnection::connect(
            UnixStream::from_std(worker_end).unwrap(),
            worker_card("test"),
        )
        .await
        .unwrap();
        let transport = worker.transport();
        for _ in 0..500 {
            if registry.is_registered(&name) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let (_task, _updates) = registry
            .submit_task(&name, DelegatedTask::new("call three agents"))
            .expect("the caller is connected");

        let finished = Arc::new(Mutex::new(Vec::new()));
        let results = Arc::new(Mutex::new(Vec::new()));
        let (order, outcomes) = (finished.clone(), results.clone());
        let agents = [
            slow.clone(),
            fast.clone(),
            mid.clone(),
            "nobody".to_string(),
        ];
        tokio::time::timeout(
            Duration::from_secs(10),
            worker.serve_one_task(move |_task, _sink, _inputs| async move {
                let runs = agents.iter().map(|agent| {
                    let (transport, order) = (transport.clone(), order.clone());
                    async move {
                        let result = run_call(transport.as_ref(), agent).await;
                        order.lock().unwrap().push(agent.clone());
                        (agent.clone(), result)
                    }
                });
                *outcomes.lock().unwrap() = futures::future::join_all(runs).await;
                Ok(())
            }),
        )
        .await
        .expect("the calls finish")
        .expect("the task ran");

        assert_eq!(
            *finished.lock().unwrap(),
            vec![
                "nobody".to_string(),
                fast.clone(),
                mid.clone(),
                slow.clone()
            ],
            "the calls finished out of the order they were made in"
        );
        for (agent, result) in results.lock().unwrap().iter() {
            if agent == "nobody" {
                assert!(
                    matches!(result, Err(CallError::UnknownAgent(a)) if a == "nobody"),
                    "{result:?}"
                );
                continue;
            }
            let outcome = result.as_ref().expect("the call succeeded");
            assert!(outcome.success);
            assert_eq!(
                outcome.response,
                format!("answer from {agent}"),
                "{agent}'s result reached {agent}'s call"
            );
        }
    }
}
