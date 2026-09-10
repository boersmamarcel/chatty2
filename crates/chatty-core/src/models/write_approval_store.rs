use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

use parking_lot::Mutex;

use crate::models::execution_approval_store::{ApprovalNotification, ApprovalResolution};

/// Decision for a filesystem write approval request
#[derive(Clone, Debug)]
pub enum WriteApprovalDecision {
    Approved,
    Denied,
}

/// Types of write operations that require approval
///
/// Pre-built API: variant fields and `is_destructive()` will be read by the
/// write approval UI (not yet wired). See `docs/pre-built-apis.md`.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub enum WriteOperation {
    /// Writing/overwriting a file
    WriteFile {
        path: String,
        is_overwrite: bool,
        content_preview: String,
    },
    /// Deleting a file
    DeleteFile { path: String },
    /// Moving/renaming a file
    MoveFile { source: String, destination: String },
    /// Applying a diff to a file
    ApplyDiff {
        path: String,
        old_preview: String,
        new_preview: String,
    },
}

impl WriteOperation {
    /// Get a human-readable description for display
    pub fn description(&self) -> String {
        match self {
            WriteOperation::WriteFile {
                path, is_overwrite, ..
            } => {
                if *is_overwrite {
                    format!("Overwrite file: {}", path)
                } else {
                    format!("Create file: {}", path)
                }
            }
            WriteOperation::DeleteFile { path } => format!("Delete file: {}", path),
            WriteOperation::MoveFile {
                source,
                destination,
            } => format!("Move: {} → {}", source, destination),
            WriteOperation::ApplyDiff { path, .. } => format!("Edit file: {}", path),
        }
    }

    /// Whether this is a destructive operation (delete, overwrite)
    ///
    /// Pre-built API: will be called by the write approval UI (not yet wired).
    #[allow(dead_code)]
    pub fn is_destructive(&self) -> bool {
        matches!(
            self,
            WriteOperation::DeleteFile { .. }
                | WriteOperation::WriteFile {
                    is_overwrite: true,
                    ..
                }
        )
    }
}

/// Request for user approval to perform a filesystem write operation
///
/// Pre-built API: `id` and `operation` will be read by the write approval UI
/// (not yet wired). See `docs/pre-built-apis.md`.
#[allow(dead_code)]
pub struct WriteApprovalRequest {
    /// Unique ID for tracking this request
    pub id: String,
    /// The operation to be approved
    pub operation: WriteOperation,
    /// Channel to send approval decision back to waiting tool
    pub responder: oneshot::Sender<WriteApprovalDecision>,
}

/// Inner state behind `PendingWriteApprovals`: the in-flight requests plus
/// the per-turn notifier the frontend installs (AGE-246 / D7) so the write
/// tools can announce a new request through the store they hold, rather than
/// a process-wide global. Shares `ApprovalNotification` with
/// `execution_approval_store` so shell/git and filesystem-write approvals
/// keep surfacing on the same notification channel/UI.
pub struct PendingWriteApprovalsState {
    // `pub(crate)`: `request_write_approval` lives in `tools::filesystem_write_tool`,
    // a sibling module, and needs direct access (mirrors how `PendingApprovalsState`
    // and `PendingClarificationsState` are used from within their own file).
    pub(crate) requests: HashMap<String, WriteApprovalRequest>,
    pub(crate) notifier: Option<mpsc::UnboundedSender<ApprovalNotification>>,
}

impl PendingWriteApprovalsState {
    fn new() -> Self {
        Self {
            requests: HashMap::new(),
            notifier: None,
        }
    }
}

/// Thread-safe storage for pending write approvals
pub type PendingWriteApprovals = Arc<Mutex<PendingWriteApprovalsState>>;

/// Per-agent store for pending filesystem write approval requests
pub struct WriteApprovalStore {
    pending_requests: PendingWriteApprovals,
    resolution_notifier: Option<mpsc::UnboundedSender<ApprovalResolution>>,
}

impl WriteApprovalStore {
    pub fn new() -> Self {
        Self {
            pending_requests: Arc::new(Mutex::new(PendingWriteApprovalsState::new())),
            resolution_notifier: None,
        }
    }

    /// Get a clone of the pending approvals handle for passing to async contexts
    pub fn get_pending_approvals(&self) -> PendingWriteApprovals {
        self.pending_requests.clone()
    }

    /// Set the notification channels for the current turn. The request
    /// notifier lives on `PendingWriteApprovals` itself, since that is the
    /// handle `request_write_approval` actually holds (AGE-246 / D7); the
    /// resolution notifier lives here, since that is what `resolve` holds.
    ///
    /// Both, and not just the first: a store that can announce a request but
    /// not its answer leaves every client that learns the outcome from the
    /// event stream stuck on a live prompt (AGE-346). This mirrors
    /// `ExecutionApprovalStore::set_notifiers` because the two stores feed
    /// one stream and one UI.
    pub fn set_notifiers(
        &mut self,
        approval_tx: mpsc::UnboundedSender<ApprovalNotification>,
        resolution_tx: mpsc::UnboundedSender<ApprovalResolution>,
    ) {
        self.pending_requests.lock().notifier = Some(approval_tx);
        self.resolution_notifier = Some(resolution_tx);
    }

    /// Resolve an approval request by ID, returning whether it existed.
    pub fn resolve(&self, id: &str, decision: WriteApprovalDecision) -> bool {
        let mut state = self.pending_requests.lock();
        if let Some(request) = state.requests.remove(id) {
            let approved = matches!(decision, WriteApprovalDecision::Approved);
            let _ = request.responder.send(decision);

            // Tell the stream the prompt is answered. Both decisions, not
            // just denial: a client that only hears about denials cannot
            // retire an approved prompt (AGE-346).
            if let Some(tx) = &self.resolution_notifier {
                let _ = tx.send(ApprovalResolution {
                    id: id.to_string(),
                    approved,
                });
            }

            true
        } else {
            false
        }
    }
}

impl Default for WriteApprovalStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::models::execution_settings::ApprovalMode;
    use crate::tools::filesystem_write_tool::request_write_approval;

    fn a_write() -> WriteOperation {
        WriteOperation::WriteFile {
            path: "/tmp/notes.md".to_string(),
            is_overwrite: false,
            content_preview: "hello".to_string(),
        }
    }

    /// AGE-346. The store could announce a write prompt but not its answer:
    /// `set_notifier` wired only the request side, so `resolve` had nowhere to
    /// report to and no `ApprovalResolved` ever reached the stream. A client
    /// that learns the outcome from the wire — chatty-web does; gpui resolves
    /// its own card on click and so never saw this — was left with a live
    /// prompt over a tool that had already run.
    ///
    /// Both decisions, because the old shape failed both. It only *looked*
    /// like an approve-only bug: the denial that appeared to work was an
    /// execution approval, whose store has always notified.
    #[tokio::test]
    async fn resolving_a_write_approval_notifies_the_stream() {
        for (decision, expected) in [
            (WriteApprovalDecision::Approved, true),
            (WriteApprovalDecision::Denied, false),
        ] {
            let mut store = WriteApprovalStore::new();
            let (approval_tx, mut approvals) = mpsc::unbounded_channel();
            let (resolution_tx, mut resolutions) = mpsc::unbounded_channel();
            store.set_notifiers(approval_tx, resolution_tx);

            let pending = store.get_pending_approvals();
            let waiter = tokio::spawn(async move {
                request_write_approval(&pending, &ApprovalMode::AlwaysAsk, a_write()).await
            });

            let requested = approvals.recv().await.expect("the prompt is announced");

            assert!(store.resolve(&requested.id, decision));

            let resolved = resolutions
                .recv()
                .await
                .expect("the answer is announced too");
            assert_eq!(resolved.id, requested.id);
            assert_eq!(resolved.approved, expected);
            assert_eq!(waiter.await.unwrap().unwrap(), expected);
        }
    }

    /// Resolving an id the store never held must not invent a resolution:
    /// a client would retire a prompt that is still parked.
    #[tokio::test]
    async fn an_unknown_id_notifies_nothing() {
        let mut store = WriteApprovalStore::new();
        let (approval_tx, _approvals) = mpsc::unbounded_channel();
        let (resolution_tx, mut resolutions) = mpsc::unbounded_channel();
        store.set_notifiers(approval_tx, resolution_tx);

        assert!(!store.resolve("never-issued", WriteApprovalDecision::Approved));
        assert!(resolutions.try_recv().is_err());
    }
}
