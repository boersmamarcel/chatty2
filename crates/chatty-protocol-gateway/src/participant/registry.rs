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

use serde_json::Value;

use super::origin::AgentOrigin;
use super::protocol::{
    BrokerFrame, InputRequest, ParticipantCard, ParticipantFrame, TaskInput, TaskState,
};

/// One update on an open task, as the HTTP side consumes it.
///
/// This is the A2A event model minus its JSON-RPC envelope: the handler adds
/// the envelope because only it knows the request's `id`.
#[derive(Debug, Clone)]
pub enum TaskUpdate {
    Status {
        state: TaskState,
        message: Option<String>,
        /// Opaque, forwarded to the A2A status's `metadata` (see
        /// [`ParticipantFrame::Status`](super::protocol::ParticipantFrame)).
        metadata: Option<Value>,
        /// What an `input-required` task is waiting for; answered through
        /// [`ParticipantRegistry::answer_task`].
        input: Option<InputRequest>,
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

/// Why an answer could not be delivered.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AnswerError {
    #[error("task '{0}' is not open on any participant")]
    UnknownTask(String),
    #[error("the participant owning task '{0}' is disconnecting")]
    ParticipantGone(String),
}

/// Why a registration was refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegisterError {
    #[error("a participant card must carry a name")]
    MissingName,
    #[error("participant '{0}' is already registered")]
    DuplicateName(String),
}

/// A registered agent as the broker serves it: what it says about itself,
/// plus where the broker knows it came from (ADR-0011 C5).
#[derive(Debug, Clone)]
pub struct RegisteredAgent {
    pub card: ParticipantCard,
    pub origin: AgentOrigin,
}

struct Participant {
    card: ParticipantCard,
    /// Where this registration arrived from. The participant does not get a
    /// say — see [`AgentOrigin`].
    origin: AgentOrigin,
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
    /// `origin` is the transport's, not the participant's: a Unix socket on
    /// this machine registers [`AgentOrigin::Local`], a leased microVM's vsock
    /// registers [`AgentOrigin::Fleet`].
    ///
    /// The name is claimed until [`deregister`](Self::deregister); a second
    /// participant offering the same one is refused rather than replacing it,
    /// because replacing would silently strand the first one's open tasks.
    pub fn register(
        &self,
        card: ParticipantCard,
        origin: AgentOrigin,
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
                origin,
                outbound,
                tasks: HashMap::new(),
            },
        );
        drop(inner);

        info!(participant = %name, %origin, "Participant registered");
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
                metadata: None,
                input: None,
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

    /// Where `name` came from, if it is registered.
    pub fn origin(&self, name: &str) -> Option<AgentOrigin> {
        self.lock().participants.get(name).map(|p| p.origin)
    }

    /// Every registered agent, in [`names`](Self::names) order.
    pub fn agents(&self) -> Vec<RegisteredAgent> {
        let inner = self.lock();
        let mut agents: Vec<RegisteredAgent> = inner
            .participants
            .values()
            .map(|p| RegisteredAgent {
                card: p.card.clone(),
                origin: p.origin,
            })
            .collect();
        agents.sort_by(|a, b| a.card.name.cmp(&b.card.name));
        agents
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

    /// Whether `task_id` is open on some participant.
    ///
    /// The A2A handler asks this to tell a `message/send` that answers a
    /// parked task from one that starts a new task: only an id the broker
    /// itself minted, and has not closed, can be the former.
    pub fn owns_task(&self, task_id: &str) -> bool {
        self.lock()
            .participants
            .values()
            .any(|p| p.tasks.contains_key(task_id))
    }

    /// Deliver the answer to a task parked in `input-required`.
    ///
    /// Addressed by task id alone: the caller answered on the A2A task it
    /// was streaming, and a runner's task is served by a worker whose
    /// participant name the caller never learned. The task stays open — the
    /// participant's next status un-parks it.
    pub fn answer_task(&self, task_id: &str, input: TaskInput) -> Result<(), AnswerError> {
        let mut inner = self.lock();
        let Some((name, participant)) = inner
            .participants
            .iter_mut()
            .find(|(_, p)| p.tasks.contains_key(task_id))
        else {
            return Err(AnswerError::UnknownTask(task_id.to_string()));
        };
        let name = name.clone();
        if participant
            .outbound
            .send(BrokerFrame::Input {
                task_id: task_id.to_string(),
                input,
            })
            .is_err()
        {
            return Err(AnswerError::ParticipantGone(task_id.to_string()));
        }
        drop(inner);
        debug!(participant = %name, task = %task_id, "Answer delivered to a parked task");
        Ok(())
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
                metadata,
                input,
            } => (
                task_id,
                TaskUpdate::Status {
                    state,
                    message,
                    metadata,
                    input,
                },
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
        register_from(reg, name, AgentOrigin::Local)
    }

    fn register_from(
        reg: &ParticipantRegistry,
        name: &str,
        origin: AgentOrigin,
    ) -> mpsc::UnboundedReceiver<BrokerFrame> {
        let (tx, rx) = mpsc::unbounded_channel();
        reg.register(card(name), origin, tx)
            .expect("registration succeeds");
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
            reg.register(card("   "), AgentOrigin::Local, tx)
                .unwrap_err(),
            RegisterError::MissingName
        );
    }

    #[test]
    fn a_duplicate_name_is_refused_rather_than_replacing_the_first() {
        let reg = ParticipantRegistry::new();
        let first = register(&reg, "worker-1");
        let (tx, _rx) = mpsc::unbounded_channel();

        assert_eq!(
            reg.register(card("worker-1"), AgentOrigin::Local, tx)
                .unwrap_err(),
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
                metadata: None,
                input: None,
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
                metadata: None,
                input: None,
            },
        );

        assert!(matches!(
            updates.recv().await,
            Some(TaskUpdate::Status { state: TaskState::Working, message: Some(m), .. }) if m == "read_file"
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
                TaskUpdate::Status { state: TaskState::Failed, message: Some(ref m), .. }
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
                metadata: None,
                input: None,
            },
        ));
    }

    #[tokio::test]
    async fn an_answer_reaches_the_participant_that_owns_the_task() {
        use super::super::protocol::{InputAnswer, InputQuestion};

        let reg = ParticipantRegistry::new();
        let mut outbound = register(&reg, "worker-1");
        let (task_id, mut updates) = reg.submit_task("worker-1", "a".into()).unwrap();
        let _ = outbound.recv().await;

        // The worker parks the task and says what it is waiting for.
        reg.on_frame(
            "worker-1",
            ParticipantFrame::Status {
                task_id: task_id.clone(),
                state: TaskState::InputRequired,
                message: Some("Which database?".into()),
                metadata: None,
                input: Some(InputRequest {
                    id: "req-1".into(),
                    questions: vec![InputQuestion {
                        id: "q1".into(),
                        question: "Which database?".into(),
                        options: vec!["Postgres".into(), "SQLite".into()],
                    }],
                }),
            },
        );
        let Some(TaskUpdate::Status {
            state: TaskState::InputRequired,
            input: Some(request),
            ..
        }) = updates.recv().await
        else {
            panic!("the caller sees the request behind the parked state");
        };
        assert_eq!(request.id, "req-1");
        assert!(reg.owns_task(&task_id), "a parked task is still open");

        // The answer is addressed by task id alone.
        let input = TaskInput {
            request_id: request.id,
            answers: vec![InputAnswer {
                id: "q1".into(),
                answer: "Postgres".into(),
                custom: false,
            }],
        };
        reg.answer_task(&task_id, input.clone()).unwrap();
        assert!(matches!(
            outbound.recv().await,
            Some(BrokerFrame::Input { task_id: t, input: i }) if t == task_id && i == input
        ));
        assert_eq!(
            reg.open_task_count("worker-1"),
            1,
            "answering does not close the task"
        );

        assert_eq!(
            reg.answer_task("task-nobody", input).unwrap_err(),
            AnswerError::UnknownTask("task-nobody".into())
        );
        assert!(!reg.owns_task("task-nobody"));
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
    fn agents_are_listed_in_name_order() {
        let reg = ParticipantRegistry::new();
        let _b = register(&reg, "b-worker");
        let _a = register(&reg, "a-worker");
        let names: Vec<String> = reg
            .agents()
            .into_iter()
            .map(|agent| agent.card.name)
            .collect();
        assert_eq!(names, vec!["a-worker".to_string(), "b-worker".to_string()]);
    }

    /// The origin is the transport's answer, and it survives to the listing —
    /// a hosted worker and a local child are both addressable, and a caller
    /// has to be able to tell them apart (ADR-0011 C5).
    #[test]
    fn each_registration_keeps_the_origin_its_transport_gave_it() {
        let reg = ParticipantRegistry::new();
        let _local = register_from(&reg, "child", AgentOrigin::Local);
        let _hosted = register_from(&reg, "leased-vm", AgentOrigin::Fleet);

        assert_eq!(reg.origin("child"), Some(AgentOrigin::Local));
        assert_eq!(reg.origin("leased-vm"), Some(AgentOrigin::Fleet));
        assert_eq!(reg.origin("nobody"), None);

        let listed: Vec<(String, AgentOrigin)> = reg
            .agents()
            .into_iter()
            .map(|agent| (agent.card.name, agent.origin))
            .collect();
        assert_eq!(
            listed,
            vec![
                ("child".to_string(), AgentOrigin::Local),
                ("leased-vm".to_string(), AgentOrigin::Fleet),
            ]
        );
    }
}
