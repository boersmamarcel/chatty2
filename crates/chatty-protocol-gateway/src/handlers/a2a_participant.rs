//! Building blocks for running a task on a local participant — registered or
//! spawned — shared by the broker's connection-based call path.
//!
//! Two things arrive here. A **registered participant** is a process that
//! connected on its own; a task is submitted straight to it. A **runner
//! agent** (ADR-0011 C2) has no process yet: the task spawns one, waits for
//! it to register, and reaps it afterwards. Past the first step the two are
//! the same code, which is why a caller cannot tell those apart either.
//!
//! Serving these over HTTP as A2A JSON-RPC/SSE was retired by BI-7: a role
//! is reached over the connection the broker made for its caller now
//! ([`crate::participant::BrokerCalls`]), never over the gateway's loopback
//! route. What is left here is the shared submit/spawn/reap machinery that
//! path still runs on.

use crate::participant::{
    DelegatedTask, ParticipantCard, ParticipantRegistry, TaskEvidence, TaskStream, VirtualAgent,
    WorkerHandle,
};
use chatty_fabric::wire::{Opaque, TaskMetadata};
use serde_json::{Value, json};
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// Agent card
// ---------------------------------------------------------------------------

/// A participant's card in the same JSON shape as a module's.
pub(crate) fn card_to_json(card: &ParticipantCard) -> Value {
    let skills: Vec<Value> = card
        .skills
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "description": s.description,
                "examples": s.examples,
            })
        })
        .collect();

    json!({
        "name": card.name,
        "displayName": card.display_name.clone().unwrap_or_else(|| card.name.clone()),
        "description": card.description,
        "version": card.version,
        "skills": skills,
        "capabilities": { "streaming": true },
    })
}

// ---------------------------------------------------------------------------
// One task in flight
// ---------------------------------------------------------------------------

/// A submitted task and everything that has to be cleaned up after it.
///
/// Shared with the broker's call path ([`crate::participant::BrokerCalls`]),
/// which runs a worker's `invoke_agent` over its connection with exactly
/// the lifecycle an A2A request gets.
pub(crate) struct RunningTask {
    pub(crate) updates: TaskStream,
    /// Cancels the task if the caller hangs up before it finishes.
    guard: TaskGuard,
    /// Present only for a worker a virtual agent started: the process or the
    /// leased microVM behind it, reaped when this is dropped.
    worker: Option<Box<dyn WorkerHandle>>,
}

impl RunningTask {
    /// The participant serving the task: the worker's node name.
    pub(crate) fn participant(&self) -> &str {
        &self.guard.participant
    }

    /// The task reached a terminal state; nothing is left to cancel.
    ///
    /// `metadata` is the terminal status's, carrying the worker's token
    /// usage. It is passed on rather than only rendered because a hosted
    /// worker's ledger row wants it beside the lease-seconds only the worker
    /// handle knows (AGE-307).
    ///
    /// Returns the runner's evidence envelope, collected after the worker
    /// was told the task is over and therefore after its worktree is
    /// committed — the count and the diff stat would be one edit stale
    /// otherwise (AGE-406).
    pub(crate) async fn finish(
        &mut self,
        succeeded: bool,
        metadata: Option<&chatty_fabric::wire::TaskMetadata>,
    ) -> Option<TaskEvidence> {
        self.guard.finished();
        let worker = self.worker.as_mut()?;
        worker.finish(succeeded, metadata);
        worker.evidence().await
    }
}

/// Fold the evidence envelope into the terminal status's `metadata`, next
/// to whatever else rides there. Evidence over the opaque-payload cap is
/// dropped from the metadata (its text is in the answer already).
pub(crate) fn with_evidence(
    metadata: Option<TaskMetadata>,
    evidence: Option<&TaskEvidence>,
) -> Option<TaskMetadata> {
    let Some(evidence) = evidence else {
        return metadata;
    };
    let data = match Opaque::from_value(&evidence.data) {
        Ok(data) => data,
        Err(error) => {
            warn!(%error, "The runner's evidence does not fit the task's metadata");
            return metadata;
        }
    };
    let mut metadata = metadata.unwrap_or_default();
    metadata.evidence = Some(data);
    Some(metadata)
}

/// Submit `task` to an already-registered participant.
pub(crate) async fn submit(
    registry: &ParticipantRegistry,
    name: &str,
    task: DelegatedTask,
) -> Option<RunningTask> {
    let (task_id, updates) = registry.submit_task(name, task).await?;
    Some(RunningTask {
        guard: TaskGuard::new(registry.clone(), name.to_string(), task_id),
        updates,
        worker: None,
    })
}

/// Start a worker for `task` and submit it.
pub(crate) async fn spawn(
    runner: &dyn VirtualAgent,
    task: DelegatedTask,
) -> Result<RunningTask, String> {
    let (worker, updates) = runner.run_task(task).await.map_err(|e| format!("{e:#}"))?;
    let task_id = worker
        .task_id()
        .expect("run_task sets the task id before returning")
        .to_string();
    Ok(RunningTask {
        guard: TaskGuard::new(
            runner.registry().clone(),
            worker.name().to_string(),
            task_id,
        ),
        updates,
        worker: Some(worker),
    })
}

// ---------------------------------------------------------------------------
// Caller liveness
// ---------------------------------------------------------------------------

/// Cancels the task if the caller goes away before it finishes.
///
/// An HTTP client that hangs up mid-stream drops the response body, which
/// drops the generator, which drops this. Without it the participant would
/// keep working — and keep holding a slot in the concurrency budget
/// (AGE-305) — for a caller that stopped listening.
struct TaskGuard {
    registry: ParticipantRegistry,
    participant: String,
    task_id: String,
    done: bool,
}

impl TaskGuard {
    fn new(registry: ParticipantRegistry, participant: String, task_id: String) -> Self {
        Self {
            registry,
            participant,
            task_id,
            done: false,
        }
    }

    fn finished(&mut self) {
        self.done = true;
    }
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        if !self.done {
            debug!(
                participant = %self.participant,
                task = %self.task_id,
                "Caller hung up; cancelling the task"
            );
            self.registry.cancel_task(&self.participant, &self.task_id);
        }
    }
}
