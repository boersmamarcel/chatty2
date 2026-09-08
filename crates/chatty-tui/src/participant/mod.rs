//! Participant mode: this process is a worker behind the broker (AGE-301).
//!
//! `chatty-tui --participant-socket <PATH> --participant-name <NAME>` is
//! `--headless` with two things changed: the prompt arrives as a broker
//! frame instead of `--message`, and the turn's events go back over the
//! socket instead of onto stderr as `CHATTY_EVENT` lines. Everything between
//! those two ends — the session, the tools, the recovery loop — is the same
//! code `--headless` runs, which is the point: a worker is not a different
//! kind of agent.
//!
//! # One task per process
//!
//! The loop reads frames until it gets a task, runs it, sends one terminal
//! status and returns. The broker's runner spawns a child per task and reaps
//! it, so a second task would never arrive; keeping the process lifecycle
//! identical to the `sub_agent` this replaces is what makes ADR-0011's
//! second kill criterion (AGE-302) a comparison of the hop rather than of
//! process-reuse strategies.
//!
//! A consequence: a `cancel` frame that arrives mid-task is not acted on
//! here — cancellation is enforced by the broker reaping the child. When
//! workers become persistent, this is the loop that grows a cancel path.

#[cfg(test)]
mod equivalence;
mod mapping;

pub use mapping::TaskMapper;

use anyhow::{Context, Result};
use chatty_core::session::SessionEvent;
use chatty_protocol_gateway::participant::{
    BrokerFrame, ParticipantCard, ParticipantConnection, ParticipantFrame, ParticipantSkill,
};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::events::AppEvent;
use crate::headless::{HeadlessRunner, run_headless};

/// Register with the broker, run one delegated task, report, exit.
pub async fn run_participant(
    mut engine: HeadlessRunner,
    event_rx: mpsc::UnboundedReceiver<AppEvent>,
    socket: &Path,
    name: &str,
) -> Result<()> {
    let mut connection = ParticipantConnection::register(socket, card(name))
        .await
        .context("failed to register with the broker")?;
    info!(
        participant = connection.name(),
        "Registered with the broker"
    );

    let (task_id, prompt) = match next_task(&mut connection).await? {
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

    engine.set_event_observer({
        let mapper = mapper.clone();
        let frames_tx = frames_tx.clone();
        Arc::new(move |event: &SessionEvent| {
            if let Some(frame) = lock(&mapper).map(event) {
                let _ = frames_tx.send(frame);
            }
        })
    });
    drop(frames_tx);

    let writer = tokio::spawn({
        let mut connection = connection;
        async move {
            while let Some(frame) = frames_rx.recv().await {
                if let Err(e) = connection.send(frame).await {
                    warn!(error = %e, "Failed to report progress to the broker");
                    return Err(e);
                }
            }
            Ok(connection)
        }
    });

    // The turn itself: the same function `--headless` runs.
    let outcome = run_headless(engine, event_rx, prompt).await;

    // The observer is dropped with the engine, which closes the queue and
    // ends the writer; only then is the socket free for the terminal status.
    let mut connection = writer
        .await
        .context("the frame writer panicked")?
        .context("the broker connection failed mid-task")?;

    let terminal = {
        let mut mapper = lock(&mapper);
        if let Err(e) = &outcome {
            // A failure `run_headless` returns never reached the session as a
            // `SessionEvent`, so the mapper has not seen it.
            mapper.map(&SessionEvent::Error(chatty_core::services::StreamError {
                kind: chatty_core::services::StreamErrorKind::Other,
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

/// Read frames until a task arrives, or the broker closes the socket.
async fn next_task(connection: &mut ParticipantConnection) -> Result<Option<(String, String)>> {
    while let Some(frame) = connection.next_frame().await? {
        match frame {
            BrokerFrame::Task { task_id, text } => return Ok(Some((task_id, text))),
            BrokerFrame::Cancel { task_id } => {
                debug!(task = %task_id, "Ignoring a cancel for a task that never started")
            }
            other => debug!(?other, "Ignoring an unexpected frame before the first task"),
        }
    }
    Ok(None)
}

/// A poisoned mapper means a panic while a frame was being built. The
/// mapper is plain owned data with no cross-field invariant, so recovering
/// it lets the task still report a terminal status instead of hanging the
/// parent on a worker that will never answer.
fn lock(mapper: &Mutex<TaskMapper>) -> std::sync::MutexGuard<'_, TaskMapper> {
    mapper.lock().unwrap_or_else(|e| e.into_inner())
}

/// What this worker publishes about itself.
///
/// Deliberately thin. A worker's skills are its tool set, which depends on
/// settings the broker already knows, and re-advertising them here would let
/// the two disagree. Live discovery is AGE-304's.
fn card(name: &str) -> ParticipantCard {
    ParticipantCard {
        name: name.to_string(),
        display_name: Some(format!("chatty worker {name}")),
        description: "A chatty agent process delegated a task by the broker.".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        skills: vec![ParticipantSkill {
            name: "delegate".to_string(),
            description: "Run a self-contained task and report the result.".to_string(),
            examples: Vec::new(),
        }],
    }
}
