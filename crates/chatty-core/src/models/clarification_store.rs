use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::{mpsc, oneshot};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// How long the agent waits for the user to answer before giving up.
/// Matches the execution-approval timeout.
///
/// Public because the delegation transport has to outlast it. A worker's
/// question travels up to a human (ADR-0011 C7) over the SSE stream that is
/// carrying the delegation, so if that stream gave up first, "nobody
/// answered" would surface as a connection error and this timeout could never
/// fire at all. `services::a2a_client` asserts that ordering at compile time
/// rather than leaving two constants to coincide.
pub const CLARIFICATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Maximum questions accepted in a single `ask_user` call. Keeps the popover
/// above the chat input answerable in one sitting.
pub const MAX_CLARIFYING_QUESTIONS: usize = 4;

/// Maximum pre-made options offered per question.
pub const MAX_QUESTION_OPTIONS: usize = 6;

fn notify_clarification(
    notifier: &Option<mpsc::UnboundedSender<ClarificationNotification>>,
    id: String,
    questions: Vec<ClarifyingQuestion>,
) {
    use tracing::warn;

    match notifier {
        Some(tx) => {
            if let Err(e) = tx.send(ClarificationNotification {
                id: id.clone(),
                questions,
            }) {
                warn!(id = %id, error = ?e, "Failed to send clarification notification");
            }
        }
        None => {
            warn!(id = %id, "Clarification notifier not set - notification not sent!");
        }
    }
}

/// A single clarifying question with its pre-made answers.
///
/// The free-text escape hatch is not part of this struct: every question
/// implicitly accepts a custom answer, and the UI always renders that entry.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ClarifyingQuestion {
    /// Stable key the model uses to match answers back to questions.
    pub id: String,
    /// The question as shown to the user.
    pub question: String,
    /// Pre-made options the user can pick with one click.
    pub options: Vec<String>,
}

/// The user's answer to one [`ClarifyingQuestion`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ClarificationAnswer {
    /// Matches [`ClarifyingQuestion::id`].
    pub id: String,
    /// The answer text — either the chosen option or what the user typed.
    pub answer: String,
    /// True when the user typed the answer instead of picking an option.
    pub custom: bool,
}

/// Notification that a clarification request was created
#[derive(Clone, Debug)]
pub struct ClarificationNotification {
    pub id: String,
    pub questions: Vec<ClarifyingQuestion>,
}

/// A pending request for the user to answer the agent's clarifying questions
pub struct ClarificationRequest {
    /// Unique ID for tracking this request
    pub id: String,
    /// The questions awaiting answers
    pub questions: Vec<ClarifyingQuestion>,
    /// When the request was created (for timeout tracking)
    pub created_at: SystemTime,
    /// Channel to send the answers back to the waiting tool
    pub responder: oneshot::Sender<Vec<ClarificationAnswer>>,
}

/// Inner state behind `PendingClarifications`: the in-flight requests plus
/// the per-turn notifier the frontend installs (AGE-246 / D7) so the
/// `ask_user` tool can announce a new request through the store it was
/// handed, rather than a process-wide global.
pub struct PendingClarificationsState {
    requests: HashMap<String, ClarificationRequest>,
    notifier: Option<mpsc::UnboundedSender<ClarificationNotification>>,
}

impl PendingClarificationsState {
    pub(crate) fn new() -> Self {
        Self {
            requests: HashMap::new(),
            notifier: None,
        }
    }
}

/// Thread-safe storage for pending clarifications (accessible from both GPUI and Tokio contexts)
pub type PendingClarifications = Arc<Mutex<PendingClarificationsState>>;

/// Ask the user one or more clarifying questions and block until they answer.
///
/// Mirrors [`request_execution_approval`](super::execution_approval_store::request_execution_approval):
/// the request is parked in `pending`, the UI is notified through the
/// per-agent notifier installed on `pending`, and the tool awaits a oneshot
/// reply so the live stream stays open.
///
/// Returns the answers, or an error on timeout or if the request was dropped
/// (which is how stream cancellation unblocks the tool).
pub async fn request_clarification(
    pending: &PendingClarifications,
    questions: Vec<ClarifyingQuestion>,
) -> anyhow::Result<Vec<ClarificationAnswer>> {
    let (tx, rx) = oneshot::channel();
    let request_id = uuid::Uuid::new_v4().to_string();

    let request = ClarificationRequest {
        id: request_id.clone(),
        questions: questions.clone(),
        created_at: SystemTime::now(),
        responder: tx,
    };

    {
        let mut state = pending.lock();
        state.requests.insert(request_id.clone(), request);
        notify_clarification(&state.notifier, request_id.clone(), questions);
    }

    match tokio::time::timeout(CLARIFICATION_TIMEOUT, rx).await {
        Ok(Ok(answers)) => Ok(answers),
        Ok(Err(_)) => {
            // Responder dropped — the request was cancelled out from under us.
            pending.lock().requests.remove(&request_id);
            Err(anyhow::anyhow!("Clarification cancelled"))
        }
        Err(_) => {
            pending.lock().requests.remove(&request_id);
            Err(anyhow::anyhow!("Clarification timeout (5 minutes)"))
        }
    }
}

/// Per-agent store for pending clarification requests.
/// Uses Arc<Mutex<>> internally to allow access from both GPUI and async Tokio contexts
#[derive(Clone)]
pub struct ClarificationStore {
    pending_requests: PendingClarifications,
}

impl ClarificationStore {
    pub fn new() -> Self {
        Self {
            pending_requests: Arc::new(Mutex::new(PendingClarificationsState::new())),
        }
    }

    /// Get a clone of the pending clarifications handle for passing to async contexts
    pub fn get_pending_clarifications(&self) -> PendingClarifications {
        self.pending_requests.clone()
    }

    /// Set the notifier for the current turn: it lives on `PendingClarifications`
    /// itself, since that is the handle `request_clarification` actually holds
    /// (AGE-246 / D7).
    pub fn set_notifier(&mut self, tx: mpsc::UnboundedSender<ClarificationNotification>) {
        self.pending_requests.lock().notifier = Some(tx);
    }

    /// Resolve a clarification request by ID, returning whether it existed.
    /// Called from the UI once the user submits their answers.
    pub fn resolve(&self, id: &str, answers: Vec<ClarificationAnswer>) -> bool {
        let mut state = self.pending_requests.lock();
        if let Some(request) = state.requests.remove(id) {
            let _ = request.responder.send(answers);
            true
        } else {
            false
        }
    }

    /// Drop every pending request, unblocking each waiting tool with an error.
    /// Called when a stream is cancelled so a pending question cannot wedge it.
    pub fn cancel_all(&self) {
        let mut state = self.pending_requests.lock();
        state.requests.clear();
    }
}

impl Default for ClarificationStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(id: &str) -> ClarifyingQuestion {
        ClarifyingQuestion {
            id: id.to_string(),
            question: "Which database?".to_string(),
            options: vec!["Postgres".to_string(), "SQLite".to_string()],
        }
    }

    #[tokio::test]
    async fn resolve_delivers_answers_to_the_waiting_tool() {
        let store = ClarificationStore::new();
        let pending = store.get_pending_clarifications();

        let waiter = tokio::spawn({
            let pending = pending.clone();
            async move { request_clarification(&pending, vec![question("q1")]).await }
        });

        // Wait for the request to be parked before resolving it.
        let id = loop {
            if let Some(id) = pending.lock().requests.keys().next().cloned() {
                break id;
            }
            tokio::task::yield_now().await;
        };

        assert!(store.resolve(
            &id,
            vec![ClarificationAnswer {
                id: "q1".to_string(),
                answer: "Postgres".to_string(),
                custom: false,
            }]
        ));

        let answers = waiter.await.unwrap().unwrap();
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].answer, "Postgres");
        assert!(!answers[0].custom);
        assert!(
            pending.lock().requests.is_empty(),
            "resolved request must be removed"
        );
    }

    #[test]
    fn resolve_reports_unknown_ids() {
        let store = ClarificationStore::new();
        assert!(!store.resolve("does-not-exist", vec![]));
    }

    /// Stream cancellation drops the pending requests; the tool must come back
    /// with an error rather than hanging until the timeout.
    #[tokio::test]
    async fn cancel_all_unblocks_waiting_tools() {
        let store = ClarificationStore::new();
        let pending = store.get_pending_clarifications();

        let waiter = tokio::spawn({
            let pending = pending.clone();
            async move { request_clarification(&pending, vec![question("q1")]).await }
        });

        loop {
            if !pending.lock().requests.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }

        store.cancel_all();

        let err = waiter.await.unwrap().unwrap_err();
        assert!(err.to_string().contains("cancelled"), "got: {err}");
    }

    /// AGE-246 / D7: two agents, each with its own `ClarificationStore`, must
    /// not cross-notify — a request made against agent A's store is
    /// delivered only to A's receiver, never B's.
    #[tokio::test]
    async fn per_agent_notifier_does_not_cross_notify_another_agents_store() {
        let mut store_a = ClarificationStore::new();
        let mut store_b = ClarificationStore::new();

        let (tx_a, mut rx_a) = mpsc::unbounded_channel();
        store_a.set_notifier(tx_a);
        let (tx_b, mut rx_b) = mpsc::unbounded_channel();
        store_b.set_notifier(tx_b);

        let pending_a = store_a.get_pending_clarifications();
        let waiter = tokio::spawn({
            let pending_a = pending_a.clone();
            async move { request_clarification(&pending_a, vec![question("q1")]).await }
        });

        let notification = rx_a
            .recv()
            .await
            .expect("agent A's receiver must see the notification");
        assert!(
            rx_b.try_recv().is_err(),
            "agent B's receiver must not see agent A's request"
        );

        assert!(store_a.resolve(
            &notification.id,
            vec![ClarificationAnswer {
                id: "q1".to_string(),
                answer: "SQLite".to_string(),
                custom: false,
            }]
        ));
        waiter.await.unwrap().unwrap();
    }
}
