use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

use parking_lot::Mutex;

use crate::models::execution_approval_store::ApprovalNotification;

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
}

impl WriteApprovalStore {
    pub fn new() -> Self {
        Self {
            pending_requests: Arc::new(Mutex::new(PendingWriteApprovalsState::new())),
        }
    }

    /// Get a clone of the pending approvals handle for passing to async contexts
    pub fn get_pending_approvals(&self) -> PendingWriteApprovals {
        self.pending_requests.clone()
    }

    /// Set the notifier for the current turn: it lives on `PendingWriteApprovals`
    /// itself, since that is the handle `request_write_approval` actually holds
    /// (AGE-246 / D7).
    pub fn set_notifier(&mut self, tx: mpsc::UnboundedSender<ApprovalNotification>) {
        self.pending_requests.lock().notifier = Some(tx);
    }

    /// Resolve an approval request by ID
    pub fn resolve(&self, id: &str, decision: WriteApprovalDecision) -> bool {
        let mut state = self.pending_requests.lock();
        if let Some(request) = state.requests.remove(id) {
            let _ = request.responder.send(decision);
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
