use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// Decision for an execution approval request
#[derive(Clone, Debug)]
pub enum ApprovalDecision {
    Approved,
    Denied,
}

/// Which store an approval belongs to: a command (shell, git, browser,
/// plugin) or a filesystem write. Spelled as `human.approve`'s `kind`
/// (EN-2a).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    Exec,
    Write,
}

/// The delegated agent an approval was raised by (AGE-646): its
/// broker-assigned name and the chain of spec names it runs under, root
/// first. The broker stamps it from the asking worker's connection
/// (EN-2a), so the human's card names the agent that actually asked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalAsker {
    pub agent: String,
    pub chain: Vec<String>,
}

/// What an approval is for, in `human.approve`'s shape (EN-2a): the kind,
/// the command or the path, a write's diff stat, and — once the broker has
/// forwarded it to the root — who asked. The notification's `command` is the
/// line a card shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalDetail {
    pub kind: ApprovalKind,
    pub command_or_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_stat: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asker: Option<ApprovalAsker>,
}

impl ApprovalDetail {
    /// A command this agent's own tool wants to run.
    pub fn exec(command: impl Into<String>) -> Self {
        Self {
            kind: ApprovalKind::Exec,
            command_or_path: command.into(),
            diff_stat: None,
            asker: None,
        }
    }

    /// An approval the broker forwarded to the root (EN-2a), as its card
    /// shows it: every field [`literal`], so what the human reads is
    /// exactly what the worker sent.
    pub fn forwarded(request: chatty_fabric::ApprovalRequest) -> Self {
        Self {
            kind: match request.kind {
                chatty_fabric::ApprovalKind::Exec => ApprovalKind::Exec,
                chatty_fabric::ApprovalKind::Write => ApprovalKind::Write,
            },
            command_or_path: literal(&request.command_or_path),
            diff_stat: request.diff_stat.as_deref().map(literal),
            asker: request.asker.map(|asker| ApprovalAsker {
                agent: literal(&asker.agent),
                chain: asker.chain.iter().map(|spec| literal(spec)).collect(),
            }),
        }
    }

    /// The command or path this approval is for, with no asker folded in
    /// (AGE-751): `[shell] echo hi`, or `[write] path (+3 -1)`. This is what
    /// `ApprovalNotification::command` carries; a card that wants to name
    /// the asker reads `asker` separately, never parses this string.
    pub fn description(&self) -> String {
        match self.kind {
            ApprovalKind::Exec => self.command_or_path.clone(),
            ApprovalKind::Write => match &self.diff_stat {
                Some(stat) => format!("[write] {} ({stat})", self.command_or_path),
                None => format!("[write] {}", self.command_or_path),
            },
        }
    }
}

/// How many characters of one forwarded field a card shows (EN-2a); the
/// rest is cut, and the cut is marked.
pub const LITERAL_CAP: usize = 4096;

/// `text` as an approval card shows it literally (EN-2a): a line break,
/// tab or carriage return as `\n`, `\t`, `\r`; any other control
/// character (an escape sequence's ESC included), a bidi override or
/// isolate, or an invisible format character as `\u{…}`; and at most
/// [`LITERAL_CAP`] characters, the rest replaced by a marker saying how
/// many were cut. So nothing a worker sends can reorder, hide or restyle
/// what the human reads.
pub fn literal(text: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(text.len().min(LITERAL_CAP));
    for (shown, c) in text.chars().enumerate() {
        if shown == LITERAL_CAP {
            let cut = text.chars().count() - LITERAL_CAP;
            let _ = write!(out, "… [{cut} more characters not shown]");
            break;
        }
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() || hidden(c) => {
                let _ = write!(out, "\\u{{{:04x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// A character that changes how the text around it is shown without being
/// seen itself: the bidi marks, embeddings, overrides and isolates, and the
/// zero-width and other invisible format characters.
fn hidden(c: char) -> bool {
    matches!(
        c,
        '\u{00ad}'
            | '\u{061c}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
    )
}

/// Notification that an approval request was created
#[derive(Clone, Debug)]
pub struct ApprovalNotification {
    pub id: String,
    /// The command or path only — never the asker (AGE-751): a card wraps
    /// this whole string as the thing being run/written, so folding
    /// `"agent asks: "` in here would land inside the backticks.
    pub command: String,
    pub is_sandboxed: bool,
    pub detail: ApprovalDetail,
}

/// Notification that an approval was resolved
#[derive(Clone, Debug)]
pub struct ApprovalResolution {
    pub id: String,
    pub approved: bool,
}

/// Inner state behind `PendingApprovals`: the in-flight requests plus the
/// per-turn notifiers the frontend installs (AGE-246 / D7) so a tool can
/// announce a new request through the store it was handed, rather than a
/// process-wide global that the next turn — on any conversation — would
/// silently overwrite.
///
/// Each pending request is just the channel to send its decision back to the
/// waiting execution; the id/command/is_sandboxed metadata a UI needs is
/// already carried separately by `ApprovalNotification`, sent at the same
/// time a request is inserted here. The resolution notifier lives here too,
/// so every clone of the store — a worker's input loop holds one (AGE-646)
/// — announces the answers it gives.
pub struct PendingApprovalsState {
    requests: HashMap<String, oneshot::Sender<ApprovalDecision>>,
    notifier: Option<mpsc::UnboundedSender<ApprovalNotification>>,
    resolution_notifier: Option<mpsc::UnboundedSender<ApprovalResolution>>,
}

impl PendingApprovalsState {
    pub(crate) fn new() -> Self {
        Self {
            requests: HashMap::new(),
            notifier: None,
            resolution_notifier: None,
        }
    }
}

/// Thread-safe storage for pending approvals (accessible from both GPUI and Tokio contexts)
pub type PendingApprovals = Arc<Mutex<PendingApprovalsState>>;

/// How long a request waits for its answer.
const APPROVAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Shared approval flow used by shell tools, git tools, and any future tool that
/// requires user confirmation before executing.
///
/// `label` is a human-readable description prefixed with the tool domain
/// (e.g. `"[git] commit with message: …"`, `"[shell] rm -rf /tmp"`).
///
/// Returns `Ok(true)` if approved, `Ok(false)` if denied, or an error on
/// timeout / channel failure.
pub async fn request_execution_approval(
    pending: &PendingApprovals,
    approval_mode: &crate::settings::models::execution_settings::ApprovalMode,
    label: &str,
    is_sandboxed: bool,
) -> anyhow::Result<bool> {
    use crate::settings::models::execution_settings::ApprovalMode;

    match approval_mode {
        ApprovalMode::AutoApproveAll => return Ok(true),
        ApprovalMode::AutoApproveSandboxed if is_sandboxed => return Ok(true),
        _ => {}
    }

    ask(
        pending,
        label.to_string(),
        is_sandboxed,
        ApprovalDetail::exec(label),
    )
    .await
}

/// Raise an approval the broker forwarded to the root (EN-2a) on the root's
/// own store, always asking a human: the root's [`ApprovalMode`], any
/// classifier and the asking callee's sandbox never decide a forwarded
/// approval, so the notification is never marked sandboxed. The
/// notification's `command` is [`ApprovalDetail::description`]; the asker
/// travels in `detail.asker` for the card to show separately (AGE-751).
///
/// [`ApprovalMode`]: crate::settings::models::execution_settings::ApprovalMode
///
/// Dropping the future — the turn it runs in was cancelled — withdraws the
/// request and announces it resolved (denied), so no card outlives it.
pub async fn request_relayed_execution_approval(
    pending: &PendingApprovals,
    detail: ApprovalDetail,
) -> anyhow::Result<bool> {
    let command = detail.description();
    ask(pending, command, false, detail).await
}

async fn ask(
    pending: &PendingApprovals,
    label: String,
    is_sandboxed: bool,
    detail: ApprovalDetail,
) -> anyhow::Result<bool> {
    use tracing::warn;

    let (tx, rx) = oneshot::channel();
    let request_id = uuid::Uuid::new_v4().to_string();

    {
        let mut state = pending.lock();
        state.requests.insert(request_id.clone(), tx);
        match &state.notifier {
            Some(notifier) => {
                if let Err(e) = notifier.send(ApprovalNotification {
                    id: request_id.clone(),
                    command: label,
                    is_sandboxed,
                    detail,
                }) {
                    warn!(id = %request_id, error = ?e, "Failed to send approval notification");
                }
            }
            None => {
                warn!(id = %request_id, "Approval notifier not set - notification not sent!");
            }
        }
    }

    // An answered request is already out of the store; this only acts on
    // one nobody answered.
    let _withdraw = Withdraw {
        pending,
        id: &request_id,
    };
    match tokio::time::timeout(APPROVAL_TIMEOUT, rx).await {
        Ok(Ok(ApprovalDecision::Approved)) => Ok(true),
        Ok(Ok(ApprovalDecision::Denied)) => Ok(false),
        Ok(Err(_)) => Err(anyhow::anyhow!("Approval channel closed")),
        Err(_) => Err(anyhow::anyhow!("Approval timeout (5 minutes)")),
    }
}

/// Takes a request that was never answered back out of the store — on
/// timeout, or when the waiting future is dropped — and tells the stream it
/// is over, so the card that shows it retires.
struct Withdraw<'a> {
    pending: &'a PendingApprovals,
    id: &'a str,
}

impl Drop for Withdraw<'_> {
    fn drop(&mut self) {
        let mut state = self.pending.lock();
        if state.requests.remove(self.id).is_some()
            && let Some(tx) = &state.resolution_notifier
        {
            let _ = tx.send(ApprovalResolution {
                id: self.id.to_string(),
                approved: false,
            });
        }
    }
}

/// Per-agent store for pending execution approval requests.
/// Uses Arc<Mutex<>> internally to allow access from both GPUI and async Tokio contexts
#[derive(Clone)]
pub struct ExecutionApprovalStore {
    pending_requests: PendingApprovals,
}

impl ExecutionApprovalStore {
    pub fn new() -> Self {
        Self {
            pending_requests: Arc::new(Mutex::new(PendingApprovalsState::new())),
        }
    }

    /// Get a clone of the pending approvals handle for passing to async contexts
    pub fn get_pending_approvals(&self) -> PendingApprovals {
        self.pending_requests.clone()
    }

    /// Set the notification channels on an existing store for the current
    /// turn. Both live on `PendingApprovals` itself, since that is the handle
    /// `request_execution_approval` actually holds and every clone shares.
    pub fn set_notifiers(
        &mut self,
        approval_tx: mpsc::UnboundedSender<ApprovalNotification>,
        resolution_tx: mpsc::UnboundedSender<ApprovalResolution>,
    ) {
        let mut state = self.pending_requests.lock();
        state.notifier = Some(approval_tx);
        state.resolution_notifier = Some(resolution_tx);
    }

    /// Resolve an approval request by ID, returning whether it existed
    /// This is called from GPUI context when user clicks approve/deny button
    pub fn resolve(&self, id: &str, decision: ApprovalDecision) -> bool {
        let mut state = self.pending_requests.lock();
        if let Some(responder) = state.requests.remove(id) {
            let approved = matches!(decision, ApprovalDecision::Approved);
            let _ = responder.send(decision);

            // Notify stream that approval was resolved
            if let Some(tx) = &state.resolution_notifier {
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

    /// The ids waiting on an answer.
    pub fn pending_ids(&self) -> Vec<String> {
        self.pending_requests
            .lock()
            .requests
            .keys()
            .cloned()
            .collect()
    }
}

impl Default for ExecutionApprovalStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::models::execution_settings::ApprovalMode;

    /// AGE-246 / D7: two agents, each with its own `ExecutionApprovalStore`,
    /// must not cross-notify — a request made against agent A's store is
    /// delivered only to A's receiver, never B's.
    #[tokio::test]
    async fn per_agent_notifier_does_not_cross_notify_another_agents_store() {
        let mut store_a = ExecutionApprovalStore::new();
        let mut store_b = ExecutionApprovalStore::new();

        let (tx_a, mut rx_a) = mpsc::unbounded_channel();
        let (resolution_tx_a, _resolution_rx_a) = mpsc::unbounded_channel();
        store_a.set_notifiers(tx_a, resolution_tx_a);

        let (tx_b, mut rx_b) = mpsc::unbounded_channel();
        let (resolution_tx_b, _resolution_rx_b) = mpsc::unbounded_channel();
        store_b.set_notifiers(tx_b, resolution_tx_b);

        let pending_a = store_a.get_pending_approvals();
        let waiter = tokio::spawn({
            let pending_a = pending_a.clone();
            async move {
                request_execution_approval(
                    &pending_a,
                    &ApprovalMode::AlwaysAsk,
                    "rm -rf /tmp",
                    false,
                )
                .await
            }
        });

        let notification = rx_a
            .recv()
            .await
            .expect("agent A's receiver must see the notification");
        assert_eq!(notification.command, "rm -rf /tmp");
        assert!(
            rx_b.try_recv().is_err(),
            "agent B's receiver must not see agent A's request"
        );

        assert!(store_a.resolve(&notification.id, ApprovalDecision::Approved));
        assert!(waiter.await.unwrap().unwrap());
    }

    #[tokio::test]
    async fn auto_approve_all_skips_the_notifier_entirely() {
        let store = ExecutionApprovalStore::new();
        let pending = store.get_pending_approvals();

        let approved =
            request_execution_approval(&pending, &ApprovalMode::AutoApproveAll, "echo hi", false)
                .await
                .unwrap();

        assert!(approved);
    }

    #[test]
    fn resolve_reports_unknown_ids() {
        let store = ExecutionApprovalStore::new();
        assert!(!store.resolve("does-not-exist", ApprovalDecision::Approved));
    }
}
