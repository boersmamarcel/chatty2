//! Participant mode: this process is a worker behind the broker (AGE-301).
//!
//! `chatty-tui --participant-socket <PATH> --participant-name <NAME>` is
//! `--headless` with two things changed: the prompt arrives as a broker
//! frame instead of `--message`, and the turn's events go back over the
//! socket rather than nowhere at all. Everything between
//! those two ends — the session, the tools, the recovery loop — is the same
//! code `--headless` runs, which is the point: a worker is not a different
//! kind of agent.
//!
//! The loop itself, the frame ordering and the `SessionEvent` → A2A table
//! are `chatty_protocol_gateway::worker`'s, not this crate's: a microVM's
//! `chatty-server` is a worker too (AGE-307) and the parent must not be able
//! to tell the two apart. All that is left here is the hole the shared loop
//! leaves for the turn, filled with the headless runner.

#[cfg(test)]
mod equivalence;
#[cfg(test)]
mod input_required_chain;

use anyhow::{Context, Result};
use chatty_protocol_gateway::worker::{answer_clarifications, serve_one_task, worker_card};
use std::path::Path;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use crate::events::AppEvent;
use crate::headless::{HeadlessRunner, run_headless};

/// Register with the broker, run one delegated task, report, exit.
pub async fn run_participant(
    mut engine: HeadlessRunner,
    event_rx: mpsc::UnboundedReceiver<AppEvent>,
    socket: &Path,
    name: &str,
) -> Result<()> {
    let stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("failed to reach the broker at {}", socket.display()))?;

    let card = worker_card(name, env!("CARGO_PKG_VERSION"));
    // A desktop worker runs as whoever launched it; the task's bearer is
    // for a hosted worker (AGE-371) and is not read here.
    serve_one_task(stream, card, move |task, sink, inputs| async move {
        // The observer is dropped with the engine, which `run_headless`
        // consumes — that is what closes the shared loop's frame queue.
        engine.set_event_observer(sink);
        // A question this turn asks goes up the chain as `input-required`;
        // the answer comes back down here and lands on the store the
        // turn's `ask_user` is waiting on (AGE-306).
        tokio::spawn(answer_clarifications(
            inputs,
            engine.session.clarifications().clone(),
        ));
        run_headless(engine, event_rx, task.text).await
    })
    .await
}
