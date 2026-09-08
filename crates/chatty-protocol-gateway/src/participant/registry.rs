//! Who is registered, and where each open task's updates go.
//!
//! The registry is the broker's memory for the process lifetime: ADR-0011
//! puts discovery, liveness and accounting in one place, and this is that
//! place for local participants. Nothing here is persisted — a participant is
//! a live connection, so an entry that outlived its socket would be a lie.
//!
//! # Locking
//!
//! One `Mutex` over the whole map, held only for map surgery — never across
//! an await. The traffic is one small frame per progress event on a
//! single-digit number of participants; a finer-grained scheme would buy
//! nothing and cost the invariant that a deregistration cannot interleave
//! with a task submission.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::protocol::{BrokerFrame, ParticipantCard, ParticipantFrame, TaskState};

/// One update on an open task, as the HTTP side consumes it.
///
/// This is the A2A event model minus its JSON-RPC envelope: the handler adds
/// the envelope because only it knows the request's `id`.
#[derive(Debug, Clone)]
pub enum TaskUpdate {
    Status {
        state: TaskState,
        message: Option<String>,
    },
    Artifact {
        text: String,
        last_chunk: bool,
    },
}

/// The stream of updates for one submitted task.
///
/// Ends when a terminal status arrives, or when the participant's socket
/// closes — [`ParticipantRegistry::deregister`] pushes a `failed` status
/// before dropping the sender, so a caller is never left waiting on a
/// process that has gone away.
pub type TaskStream = mpsc::UnboundedReceiver<TaskUpdate>;

/// Why a registration was refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegisterError {
    #[error("a participant card must carry a name")]
    MissingName,
    #[error("participant '{0}' is already registered")]
    DuplicateName(String),
}

struct Participant {
    card: ParticipantCard,
    /// Frames queued for this participant's socket writer.
    outbound: mpsc::UnboundedSender<BrokerFrame>,
    /// Open tasks: id → where this task's updates go.
    tasks: HashMap<String, mpsc::UnboundedSender<TaskUpdate>>,
}

#[derive(Default)]
struct Inner {
    participants: HashMap<String, Participant>,
}

/// The broker's live local participants. Cheap to clone; all clones share
/// one map.
#[derive(Clone, Default)]
pub struct ParticipantRegistry {
    inner: Arc<Mutex<Inner>>,
    next_task: Arc<AtomicU64>,
}

impl ParticipantRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a connection's card and its outbound queue.
    ///
    /// The name is claimed until [`deregister`](Self::deregister); a second
    /// participant offering the same one is refused rather than replacing it,
    /// because replacing would silently strand the first one's open tasks.
    pub fn register(
        &self,
        card: ParticipantCard,
        outbound: mpsc::UnboundedSender<BrokerFrame>,
    ) -> Result<String, RegisterError> {
        let name = card.name.trim().to_string();
        if name.is_empty() {
            return Err(RegisterError::MissingName);
        }

        let mut inner = self.lock();
        if inner.participants.contains_key(&name) {
            return Err(RegisterError::DuplicateName(name));
        }
        inner.participants.insert(
            name.clone(),
            Participant {
                card: ParticipantCard {
                    name: name.clone(),
                    ..card
                },
                outbound,
                tasks: HashMap::new(),
            },
        );
        drop(inner);

        info!(participant = %name, "Local participant registered");
        Ok(name)
    }

    /// Drop a participant and fail everything it still owed.
    ///
    /// Called when the socket closes, however it closed — a clean exit and a
    /// crash are the same event from here, which is the point: liveness is
    /// the connection, not a heartbeat the participant could lie about.
    pub fn deregister(&self, name: &str) {
        let Some(participant) = self.lock().participants.remove(name) else {
            return;
        };

        let open = participant.tasks.len();
        for (task_id, sink) in participant.tasks {
            debug!(participant = %name, task = %task_id, "Failing a task whose participant went away");
            let _ = sink.send(TaskUpdate::Status {
                state: TaskState::Failed,
                message: Some(format!("participant '{name}' disconnected")),
            });
        }

        info!(participant = %name, failed_tasks = open, "Local participant deregistered");
    }

    /// Names of every registered participant, sorted so listings are stable.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.lock().participants.keys().cloned().collect();
        names.sort();
        names
    }

    pub fn is_registered(&self, name: &str) -> bool {
        self.lock().participants.contains_key(name)
    }

    pub fn card(&self, name: &str) -> Option<ParticipantCard> {
        self.lock().participants.get(name).map(|p| p.card.clone())
    }

    /// Every card, in [`names`](Self::names) order.
    pub fn cards(&self) -> Vec<ParticipantCard> {
        let inner = self.lock();
        let mut cards: Vec<ParticipantCard> = inner
            .participants
            .values()
            .map(|p| p.card.clone())
            .collect();
        cards.sort_by(|a, b| a.name.cmp(&b.name));
        cards
    }

    /// Hand `text` to `name` as a new task.
    ///
    /// Returns the task's id and its update stream, or `None` if the
    /// participant is not registered or its socket writer has already gone.
    pub fn submit_task(&self, name: &str, text: String) -> Option<(String, TaskStream)> {
        let task_id = format!(
            "task-{}-{}",
            crate::gateway::new_id(),
            self.next_task.fetch_add(1, Ordering::Relaxed)
        );
        let (tx, rx) = mpsc::unbounded_channel();

        let mut inner = self.lock();
        let participant = inner.participants.get_mut(name)?;
        if participant
            .outbound
            .send(BrokerFrame::Task {
                task_id: task_id.clone(),
                text,
            })
            .is_err()
        {
            // The writer task is gone; the read loop's deregister is on its
            // way. Don't hand the caller a stream nothing will ever feed.
            warn!(participant = %name, "Dropping a task: the participant's socket is closing");
            return None;
        }
        participant.tasks.insert(task_id.clone(), tx);
        drop(inner);

        debug!(participant = %name, task = %task_id, "Task submitted to a local participant");
        Some((task_id, rx))
    }

    /// Tell a participant to stop, and forget the task.
    ///
    /// Used when a caller hangs up mid-task. No reply is expected: the
    /// participant's own terminal status, if it sends one, lands on a task
    /// that is already gone and is dropped by [`on_frame`](Self::on_frame).
    pub fn cancel_task(&self, name: &str, task_id: &str) {
        let mut inner = self.lock();
        let Some(participant) = inner.participants.get_mut(name) else {
            return;
        };
        if participant.tasks.remove(task_id).is_some() {
            let _ = participant.outbound.send(BrokerFrame::Cancel {
                task_id: task_id.to_string(),
            });
            debug!(participant = %name, task = %task_id, "Task cancelled");
        }
    }

    /// Route one frame from `name`'s socket to the task it names.
    ///
    /// A frame for an unknown task is dropped with a log line rather than
    /// killing the connection: the caller may simply have hung up first, and
    /// a participant is not required to notice before its next frame.
    ///
    /// [`ParticipantFrame::Register`] is handled by the connection loop
    /// before any task traffic; a second one arriving here is a protocol
    /// error and is reported as `false`.
    pub fn on_frame(&self, name: &str, frame: ParticipantFrame) -> bool {
        let (task_id, update, terminal) = match frame {
            ParticipantFrame::Register { .. } => {
                warn!(participant = %name, "Ignoring a second register frame on one connection");
                return false;
            }
            ParticipantFrame::Status {
                task_id,
                state,
                message,
            } => (
                task_id,
                TaskUpdate::Status { state, message },
                state.is_terminal(),
            ),
            ParticipantFrame::Artifact {
                task_id,
                text,
                last_chunk,
            } => (task_id, TaskUpdate::Artifact { text, last_chunk }, false),
        };

        let mut inner = self.lock();
        let Some(participant) = inner.participants.get_mut(name) else {
            return true;
        };
        // A terminal status is the task's last update, so the entry is taken
        // out before the send: the sink is dropped with this scope, which is
        // what ends the caller's stream.
        let sink = if terminal {
            participant.tasks.remove(&task_id)
        } else {
            participant.tasks.get(&task_id).cloned()
        };
        drop(inner);

        match sink {
            Some(sink) => {
                let _ = sink.send(update);
            }
            None => debug!(
                participant = %name,
                task = %task_id,
                "Dropping an update for a task nobody is waiting on"
            ),
        }
        true
    }

    /// Open task count for `name` — liveness assertions in tests, and the
    /// per-endpoint budget AGE-305 will meter.
    pub fn open_task_count(&self, name: &str) -> usize {
        self.lock()
            .participants
            .get(name)
            .map(|p| p.tasks.len())
            .unwrap_or(0)
    }

    /// A poisoned lock means a panic happened while the map was being
    /// edited. The map is a plain `HashMap` of owned values with no
    /// cross-entry invariant, so recovering it is sound and strictly better
    /// than taking the whole broker down with every participant on it.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(name: &str) -> ParticipantCard {
        ParticipantCard {
            name: name.to_string(),
            description: "a test participant".to_string(),
            ..Default::default()
        }
    }

    /// Register `name` and keep its outbound receiver alive for the caller.
    fn register(reg: &ParticipantRegistry, name: &str) -> mpsc::UnboundedReceiver<BrokerFrame> {
        let (tx, rx) = mpsc::unbounded_channel();
        reg.register(card(name), tx).expect("registration succeeds");
        rx
    }

    #[test]
    fn a_registered_participant_is_addressable_by_name() {
        let reg = ParticipantRegistry::new();
        let _outbound = register(&reg, "worker-1");

        assert!(reg.is_registered("worker-1"));
        assert_eq!(reg.names(), vec!["worker-1".to_string()]);
        assert_eq!(
            reg.card("worker-1").unwrap().description,
            "a test participant"
        );
        assert!(reg.card("nobody").is_none());
    }

    #[test]
    fn a_nameless_card_is_refused() {
        let reg = ParticipantRegistry::new();
        let (tx, _rx) = mpsc::unbounded_channel();
        assert_eq!(
            reg.register(card("   "), tx).unwrap_err(),
            RegisterError::MissingName
        );
    }

    #[test]
    fn a_duplicate_name_is_refused_rather_than_replacing_the_first() {
        let reg = ParticipantRegistry::new();
        let first = register(&reg, "worker-1");
        let (tx, _rx) = mpsc::unbounded_channel();

        assert_eq!(
            reg.register(card("worker-1"), tx).unwrap_err(),
            RegisterError::DuplicateName("worker-1".into())
        );
        // The first participant still owns the name and its queue.
        assert!(!first.is_closed());
        assert!(reg.is_registered("worker-1"));
    }

    #[tokio::test]
    async fn a_task_reaches_the_participant_and_its_updates_come_back() {
        let reg = ParticipantRegistry::new();
        let mut outbound = register(&reg, "worker-1");

        let (task_id, mut updates) = reg
            .submit_task("worker-1", "summarise foo.rs".into())
            .expect("the participant is registered");

        let BrokerFrame::Task {
            task_id: sent,
            text,
        } = outbound.recv().await.unwrap()
        else {
            panic!("expected a task frame");
        };
        assert_eq!(sent, task_id);
        assert_eq!(text, "summarise foo.rs");
        assert_eq!(reg.open_task_count("worker-1"), 1);

        reg.on_frame(
            "worker-1",
            ParticipantFrame::Status {
                task_id: task_id.clone(),
                state: TaskState::Working,
                message: Some("read_file".into()),
            },
        );
        reg.on_frame(
            "worker-1",
            ParticipantFrame::Artifact {
                task_id: task_id.clone(),
                text: "foo.rs defines Foo".into(),
                last_chunk: true,
            },
        );
        reg.on_frame(
            "worker-1",
            ParticipantFrame::Status {
                task_id: task_id.clone(),
                state: TaskState::Completed,
                message: None,
            },
        );

        assert!(matches!(
            updates.recv().await,
            Some(TaskUpdate::Status { state: TaskState::Working, message: Some(m) }) if m == "read_file"
        ));
        assert!(matches!(
            updates.recv().await,
            Some(TaskUpdate::Artifact { text, last_chunk: true }) if text == "foo.rs defines Foo"
        ));
        assert!(matches!(
            updates.recv().await,
            Some(TaskUpdate::Status {
                state: TaskState::Completed,
                ..
            })
        ));
        assert!(
            updates.recv().await.is_none(),
            "a terminal status ends the stream"
        );
        assert_eq!(
            reg.open_task_count("worker-1"),
            0,
            "a completed task is no longer open"
        );
    }

    #[tokio::test]
    async fn a_disconnect_fails_every_open_task() {
        let reg = ParticipantRegistry::new();
        let _outbound = register(&reg, "worker-1");

        let (_a, mut first) = reg.submit_task("worker-1", "a".into()).unwrap();
        let (_b, mut second) = reg.submit_task("worker-1", "b".into()).unwrap();

        reg.deregister("worker-1");

        for stream in [&mut first, &mut second] {
            let update = stream
                .recv()
                .await
                .expect("an open task is told why it died");
            assert!(matches!(
                update,
                TaskUpdate::Status { state: TaskState::Failed, message: Some(ref m) }
                    if m.contains("disconnected")
            ));
            assert!(stream.recv().await.is_none(), "then the stream ends");
        }
        assert!(!reg.is_registered("worker-1"));
        assert!(reg.names().is_empty());
    }

    #[test]
    fn a_task_for_an_unregistered_participant_is_not_accepted() {
        let reg = ParticipantRegistry::new();
        assert!(reg.submit_task("nobody", "hello".into()).is_none());
    }

    #[tokio::test]
    async fn cancelling_forgets_the_task_and_tells_the_participant() {
        let reg = ParticipantRegistry::new();
        let mut outbound = register(&reg, "worker-1");
        let (task_id, mut updates) = reg.submit_task("worker-1", "a".into()).unwrap();
        let _ = outbound.recv().await;

        reg.cancel_task("worker-1", &task_id);

        assert!(matches!(
            outbound.recv().await,
            Some(BrokerFrame::Cancel { task_id: t }) if t == task_id
        ));
        assert_eq!(reg.open_task_count("worker-1"), 0);
        assert!(updates.recv().await.is_none());

        // A late frame for the cancelled task is dropped, not fatal.
        assert!(reg.on_frame(
            "worker-1",
            ParticipantFrame::Status {
                task_id,
                state: TaskState::Completed,
                message: None,
            },
        ));
    }

    #[test]
    fn a_second_register_frame_is_a_protocol_error() {
        let reg = ParticipantRegistry::new();
        let _outbound = register(&reg, "worker-1");
        assert!(!reg.on_frame(
            "worker-1",
            ParticipantFrame::Register {
                card: card("other")
            }
        ));
    }

    #[test]
    fn cards_are_listed_in_name_order() {
        let reg = ParticipantRegistry::new();
        let _b = register(&reg, "b-worker");
        let _a = register(&reg, "a-worker");
        let names: Vec<String> = reg.cards().into_iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["a-worker".to_string(), "b-worker".to_string()]);
    }
}
