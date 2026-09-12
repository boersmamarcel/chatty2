//! An agent the broker publishes that has no participant until a task
//! arrives.
//!
//! A registered participant is a live connection: it exists first and is
//! addressed second. A *virtual* agent is the other order — the broker
//! advertises a card, and only when a task is addressed to it does something
//! come into existence to serve that task, register over the participant
//! socket, and be reaped afterwards. Past registration the two are the same
//! code, which is why a caller cannot tell them apart.
//!
//! Making that something is not this crate's business. On the desktop it is
//! a child process ([`LocalRunner`](super::LocalRunner), ADR-0011 C2); hosted
//! it is a Firecracker microVM leased for the turn, which lives in hive
//! because the Firecracker control plane does (AGE-307, C8). The broker only
//! knows that it can ask for a worker, that the worker will show up in the
//! registry under the name it is told, and that dropping the handle reaps
//! whatever was made.

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;
use serde_json::Value;

use super::protocol::{DelegatedTask, ParticipantCard};
use super::registry::{ParticipantRegistry, TaskStream};

/// The worker behind one task, reaped when this is dropped.
///
/// Reaping on `Drop` rather than on an explicit call is deliberate: the task
/// ends on several paths — completion, the caller hanging up, the broker
/// shutting down — and an orphaned worker holding a process, a worktree or a
/// lease is the failure every one of them shares.
pub trait WorkerHandle: Send {
    /// The participant name this worker registered under.
    fn name(&self) -> &str;

    /// The task it was given, once it has been submitted.
    fn task_id(&self) -> Option<&str>;

    /// Record how the task ended, before the handle is dropped.
    ///
    /// `metadata` is the terminal status's, which is where a worker's token
    /// usage rides back (A2A has no field for it, so the task protocol does
    /// not carry accounting). It is handed over here because this is the one
    /// moment both halves of a turn's cost are known in the same place: the
    /// worker reported the tokens and the host knows what the worker cost to
    /// run. ADR-0010's ledger wants both as line items on one turn.
    ///
    /// What that buys depends on the worker: a committed worktree locally, a
    /// billed lease hosted.
    fn finish(&mut self, succeeded: bool, metadata: Option<&Value>);

    /// Text to append to the worker's reported answer, e.g. naming the
    /// branch a local worktree committed to (AGE-399). `None` when there is
    /// nothing to add — the default, and every worker whose workspace was
    /// never isolated.
    fn merge_hint(&self) -> Option<&str> {
        None
    }
}

/// What [`VirtualAgent::run_task`] returns. Boxed by hand rather than through
/// `futures`, which this crate does not depend on.
pub type WorkerFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(Box<dyn WorkerHandle>, TaskStream)>> + Send + 'a>>;

/// Makes a worker per task and publishes it under one agent name.
pub trait VirtualAgent: Send + Sync {
    /// The name callers address at `/a2a/{name}`.
    fn agent_name(&self) -> &str;

    /// The card served at `/a2a/{agent_name}/.well-known/agent.json`. It
    /// describes the *kind* of worker this makes, since no particular worker
    /// exists until a task arrives.
    fn agent_card(&self) -> ParticipantCard;

    /// The registry its workers register in — the same one the gateway
    /// serves, so a started worker is reachable by name like any other.
    fn registry(&self) -> &ParticipantRegistry;

    /// Start a worker and hand it `task`.
    ///
    /// Returns once the worker has registered and the task has been
    /// submitted, so the handle's `task_id` is set. The update stream is
    /// returned alongside the handle rather than owned by it, so the caller
    /// can read updates while still holding the thing that reaps the worker.
    fn run_task(&self, task: DelegatedTask) -> WorkerFuture<'_>;
}
