//! Register, run one delegated task, report, return.
//!
//! This is `chatty-tui`'s AGE-301 participant loop with the agent lifted out
//! of it. What is left is everything the wire contract owns — the
//! registration, the frame ordering, the single terminal status — and a hole
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

use std::future::Future;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use chatty_core::services::{StreamError, StreamErrorKind};
use chatty_core::session::SessionEvent;
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
/// binary's, so a broker log says which build answered.
pub fn worker_card(name: &str, version: &str) -> ParticipantCard {
    ParticipantCard {
        name: name.to_string(),
        display_name: Some(format!("chatty worker {name}")),
        description: "A chatty agent delegated a task by the broker.".to_string(),
        version: version.to_string(),
        skills: vec![ParticipantSkill {
            name: "delegate".to_string(),
            description: "Run a self-contained task and report the result.".to_string(),
            examples: Vec::new(),
        }],
    }
}

/// Register over `stream`, run the first task that arrives through `run`, and
/// report its outcome.
///
/// `stream` is already connected to the broker: a Unix socket on the desktop,
/// a vsock stream from inside a microVM (AGE-307). Which one it is changes
/// nothing below this line, which is the property C8 is built on.
///
/// `run` is handed the task — its prompt and, when the caller presented one,
/// their bearer (AGE-371) — the sink its turn's events must go to, and the
/// receiver the broker's answers to the turn's questions arrive on; its
/// `Err` becomes the task's failure message. It must not outlive the
/// sink it was given — the sink is what closes the frame queue, and a copy
/// left alive in a detached task would hold the terminal status behind it.
/// Dropping the future that owns it, which is what awaiting `run` does, is
/// enough.
pub async fn serve_one_task<S, F, Fut>(stream: S, card: ParticipantCard, run: F) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Send + 'static,
    F: FnOnce(DelegatedTask, EventSink, InputReceiver) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    let mut connection = ParticipantConnection::register_over(stream, card)
        .await
        .context("failed to register with the broker")?;
    info!(
        participant = connection.name(),
        "Registered with the broker"
    );

    let (task_id, task) = match next_task(&mut connection).await? {
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
    let mapper = Arc::new(Mutex::new(TaskMapper::new(task_id.clone())));

    let sink: EventSink = {
        let mapper = mapper.clone();
        Arc::new(move |event: &SessionEvent| {
            if let Some(frame) = lock(&mapper).map(event) {
                let _ = frames_tx.send(frame);
            }
        })
    };

    let (reader, mut writer_half) = connection.into_split();
    let writer = tokio::spawn(async move {
        while let Some(frame) = frames_rx.recv().await {
            if let Err(e) = writer_half.send(frame).await {
                warn!(error = %e, "Failed to report progress to the broker");
                return Err(e);
            }
        }
        Ok(writer_half)
    });

    // Answers come down the read half while the turn runs. The reader lives
    // exactly as long as the turn: the receiver it feeds is dropped with the
    // turn, and nothing after that can be waiting for a frame.
    let (inputs_tx, inputs_rx) = mpsc::unbounded_channel();
    let reader = tokio::spawn(forward_inputs(reader, task_id.clone(), inputs_tx));

    let outcome = run(task, sink, inputs_rx).await;
    reader.abort();

    // The sink died with the future that owned it, which closed the queue and
    // ended the writer; only then is the socket free for the terminal status.
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

/// Read frames for the running task until the broker closes the socket or
/// nobody is left to take an answer.
async fn forward_inputs(
    mut reader: ParticipantReader,
    task_id: String,
    inputs: mpsc::UnboundedSender<crate::participant::TaskInput>,
) {
    loop {
        match reader.next_frame().await {
            Ok(Some(BrokerFrame::Input {
                task_id: for_task,
                input,
            })) if for_task == task_id => {
                if inputs.send(input).is_err() {
                    debug!(task = %task_id, "An answer arrived after the turn stopped listening");
                    return;
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
                return;
            }
            Err(e) => {
                warn!(task = %task_id, error = %e, "Lost the broker mid-task");
                return;
            }
        }
    }
}

/// Read frames until a task arrives, or the broker closes the socket.
async fn next_task(
    connection: &mut ParticipantConnection,
) -> Result<Option<(String, DelegatedTask)>> {
    while let Some(frame) = connection.next_frame().await? {
        match frame {
            BrokerFrame::Task {
                task_id,
                text,
                bearer,
            } => return Ok(Some((task_id, DelegatedTask { text, bearer }))),
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
