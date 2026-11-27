//! Every request and response shape of chatty-server's conversation sync API,
//! in one place (AGE-2088).
//!
//! Matched to what chatty-server sends (hive's
//! `services/chatty-server/src/wire.rs`: `ConversationSummary`,
//! `ConversationChanges`, `ConversationDetail`) in AGE-2157. Decoding is
//! deliberately tolerant — unknown fields are ignored, and newer fields are
//! optional — so a server a release behind or ahead still decodes.
//!
//! * `GET /api/conversations?since=<cursor>&limit=<n>` → [`ChangesPage`]:
//!   conversations changed after the cursor, oldest first, each the full
//!   summary plus `version` and `turn_status` (archived ones too, with
//!   `archived: true`); a deleted one as `{id, deleted: true}`; and
//!   `next_cursor`, which is never null in this mode and echoes the cursor
//!   sent when the page is empty. A full sync is `since=0`
//!   ([`FULL_SYNC_CURSOR`]); the plain list without `since` pages with a
//!   different cursor that must never be sent as `since`. A cursor at or
//!   before the newest tombstone the server has pruned is answered 410
//!   (`cursor_expired`); `since` combined with another list filter, or a
//!   malformed cursor, is a 422.
//! * `GET /api/conversations/{id}` → [`ConversationDetail`]: the history,
//!   the turns' `traces` and `usage` (sparse, by message index), the totals,
//!   and `version`, `placement` and `turn_status`.

use std::time::{Duration, UNIX_EPOCH};

use rig_core::completion::Message;
use serde::{Deserialize, Deserializer};

use crate::models::conversation::Placement;
use crate::models::message_types::{
    ApprovalBlock, ApprovalState, SystemTrace, ToolCallBlock, ToolCallState, ToolSource, TraceItem,
    friendly_tool_name,
};
use crate::models::token_usage::{ConversationTokenUsage, TokenUsage};
use crate::repositories::ConversationData;
use crate::services::swarm_trace::AGENTS_TRACE_KEY;

/// The `since` cursor of a full sync: every conversation and every tombstone
/// the server still holds.
pub const FULL_SYNC_CURSOR: &str = "0";

/// Changes asked for per page. A shorter page is the last one.
pub const PAGE_LIMIT: usize = 100;

/// Whether a conversation's turn is running, from the server's per-user
/// registry of running turns (HX-8). Live state: shown, never persisted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    #[default]
    Idle,
    Running,
    WaitingForYou,
    /// The server restarted mid-turn and kept the turn's state (HX-20); the
    /// user can resume it.
    Interrupted,
    /// A status this build does not know; shown as idle.
    #[serde(other)]
    Unknown,
}

/// One page of `GET /api/conversations?since=…`.
#[derive(Clone, Debug, Default)]
pub struct ChangesPage {
    /// Changed conversations, oldest first.
    pub changes: Vec<RemoteChange>,
    /// Where the next pull starts. `None` when the server sent none.
    pub next_cursor: Option<String>,
}

/// One entry of a [`ChangesPage`].
#[derive(Clone, Debug, PartialEq)]
pub enum RemoteChange {
    /// The conversation exists on the server and changed after the cursor.
    Upsert(RemoteSummary),
    /// The conversation was deleted on the server.
    Deleted { id: String },
}

impl RemoteChange {
    pub fn id(&self) -> &str {
        match self {
            RemoteChange::Upsert(summary) => &summary.id,
            RemoteChange::Deleted { id } => id,
        }
    }
}

/// A conversation as the server's list describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteSummary {
    pub id: String,
    pub title: String,
    /// The model the server runs it on, when it names one.
    pub model_id: Option<String>,
    /// Bumped by the server on every save.
    pub version: u64,
    /// Unix seconds, the unit the local rows keep.
    pub updated_at: i64,
    pub turn_status: TurnStatus,
    pub placement: Placement,
    /// The name of the device that holds the conversation, when the server
    /// says (`placement_device.name`). Live, like `turn_status`: never persisted.
    pub device_name: Option<String>,
    /// Archived on the server (AGE-1840): not listed on this device.
    pub archived: bool,
}

/// `GET /api/conversations/{id}`: the conversation, as chatty-server's
/// `ConversationDetail` sends it (AGE-2157).
///
/// The history is required; the traces, usage and totals are optional, so a
/// server that does not send them yet decodes, and
/// [`Conversation::replace_from_remote`](crate::models::conversation::Conversation::replace_from_remote)
/// keeps this device's copy of whatever is absent.
///
/// There are no attachment paths here, on purpose (AGE-2143): a path the
/// server names is a path on some other machine, and must never become one
/// this device reads or uploads.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ConversationDetail {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub model_id: String,
    /// The pre-HX-8 flag; [`turn_status`](Self::turn_status) supersedes it.
    #[serde(default)]
    pub turn_active: bool,
    #[serde(default)]
    pub turn_status: Option<TurnStatus>,
    #[serde(default)]
    pub version: Option<u64>,
    #[serde(default, deserialize_with = "placement_opt")]
    pub placement: Option<Placement>,
    /// Which device holds it, when `placement` is `"device"` (HX-13).
    #[serde(default)]
    pub placement_device: Option<PlacementDevice>,
    #[serde(default)]
    pub messages: Vec<Message>,
    /// The turns' traces, sparse: only the messages that have one, by index.
    #[serde(default)]
    pub traces: Vec<MessageTrace>,
    /// What each turn's own requests spent, sparse and by index like
    /// `traces`. chatty-server leaves it out when there is none.
    #[serde(default)]
    pub usage: Vec<MessageUsage>,
    #[serde(default)]
    pub tool_call_count: Option<u32>,
    #[serde(default)]
    pub context_tokens: Option<u32>,
    /// The conversation's time (AGE-2124): Unix seconds. chatty-server sends
    /// no other.
    #[serde(default)]
    pub last_active_at: Option<i64>,
}

/// One turn's trace in a [`ConversationDetail`]: chatty-server's projection
/// of the trace stored on the message at `message_index`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct MessageTrace {
    pub message_index: usize,
    #[serde(default)]
    pub items: Vec<TraceItemWire>,
    /// The turn's swarm (chatty-core's `AgentRecord`s), as stored under
    /// [`AGENTS_TRACE_KEY`]. Kept as sent.
    #[serde(default)]
    pub agents: Vec<serde_json::Value>,
    /// When the turn started and ended.
    #[serde(default)]
    pub times: Option<TurnTimes>,
}

/// When a turn started and ended, in Unix milliseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct TurnTimes {
    #[serde(default)]
    pub started_ms: u64,
    #[serde(default)]
    pub ended_ms: u64,
}

/// One item of a [`MessageTrace`]: the two kinds chatty-server projects.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TraceItemWire {
    ToolCall {
        #[serde(default)]
        id: String,
        name: String,
        /// The arguments, as the JSON string the tool was given.
        #[serde(default)]
        input: String,
        /// `None` for a call the turn never got an answer to.
        #[serde(default)]
        output: Option<String>,
        #[serde(default)]
        error: Option<String>,
    },
    Approval {
        id: String,
        #[serde(default)]
        command: String,
        #[serde(default)]
        is_sandboxed: bool,
        state: ApprovalStateWire,
        /// Unix seconds.
        #[serde(default)]
        created_at: Option<i64>,
    },
    /// A kind this build does not know; left out.
    #[serde(other)]
    Unknown,
}

/// How an approval in a finished turn was answered. There is no pending: a
/// trace is only on the wire once its turn is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStateWire {
    /// The turn ended without a decision.
    Unanswered,
    Approved,
    Denied,
    #[serde(other)]
    Unknown,
}

/// One turn's own usage in a [`ConversationDetail`], by the index of the
/// turn's final message.
#[derive(Clone, Debug, Deserialize)]
pub struct MessageUsage {
    pub message_index: usize,
    pub usage: TokenUsage,
}

impl ConversationDetail {
    /// Where the conversation is placed, with the holding device's id filled
    /// in from `placement_device`.
    pub fn current_placement(&self) -> Option<Placement> {
        self.placement
            .clone()
            .map(|placement| with_device(placement, self.placement_device.as_ref()))
    }

    /// Is a turn running on the server, by either the HX-8 status or the
    /// older flag?
    pub fn turn_running(&self) -> bool {
        match self.turn_status {
            Some(status) => matches!(status, TurnStatus::Running | TurnStatus::WaitingForYou),
            None => self.turn_active,
        }
    }

    /// The row this detail describes, as this device stores it, under
    /// `local_id`. A column the server did not send is written empty, which
    /// is what tells `replace_from_remote` to keep this device's copy of it.
    /// Attachment paths are always empty: they never come from the server.
    /// The sync columns are left empty: the caller sets the sync state.
    pub fn to_conversation_data(&self, local_id: &str) -> ConversationData {
        fn json<T: serde::Serialize>(value: &T, empty: &str) -> String {
            serde_json::to_string(value).unwrap_or_else(|_| empty.to_string())
        }
        let (traces, timestamps) = self.per_message();
        let token_usage = if self.usage.is_empty() {
            "{}".to_string()
        } else {
            let mut total = ConversationTokenUsage::new();
            for line in &self.usage {
                total.add_usage(line.usage.clone());
            }
            json(&total, "{}")
        };
        ConversationData {
            id: local_id.to_string(),
            title: self.title.clone(),
            model_id: self.model_id.clone(),
            message_history: json(&self.messages, "[]"),
            system_traces: json(&traces, "[]"),
            token_usage,
            attachment_paths: "[]".to_string(),
            message_timestamps: json(&timestamps, "[]"),
            message_feedback: "[]".to_string(),
            regeneration_records: "[]".to_string(),
            // The server sends no creation time; 0 tells `replace_from_remote`
            // to keep this device's own.
            created_at: 0,
            updated_at: self.last_active_at.unwrap_or(0),
            working_dir: None,
            agent_task_snapshot: None,
            sync: None,
            placement: None,
            remote_version: None,
            last_synced_at: None,
            tool_call_count: self.tool_call_count.unwrap_or(0),
            context_tokens: self.context_tokens.unwrap_or(0),
        }
    }

    /// The sparse `traces` as this device stores them, one slot per message:
    /// each as a [`SystemTrace`] (with the swarm under [`AGENTS_TRACE_KEY`]),
    /// and the time the turn ended as its message's timestamp. Both are empty
    /// when the server sent no trace, so nothing is invented.
    fn per_message(&self) -> (Vec<Option<serde_json::Value>>, Vec<Option<i64>>) {
        if self.traces.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let len = self.messages.len();
        let mut traces = vec![None; len];
        let mut timestamps = vec![None; len];
        for trace in self.traces.iter().filter(|trace| trace.message_index < len) {
            let index = trace.message_index;
            let system_trace = SystemTrace {
                items: trace.items.iter().filter_map(trace_item).collect(),
                ..SystemTrace::default()
            };
            let mut value = match serde_json::to_value(&system_trace) {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!(?error, index, "could not store a server trace");
                    continue;
                }
            };
            if !trace.agents.is_empty()
                && let Some(object) = value.as_object_mut()
            {
                object.insert(
                    AGENTS_TRACE_KEY.to_string(),
                    serde_json::Value::Array(trace.agents.clone()),
                );
            }
            traces[index] = Some(value);
            timestamps[index] = trace
                .times
                .filter(|times| times.ended_ms > 0)
                .map(|times| (times.ended_ms / 1_000) as i64);
        }
        (traces, timestamps)
    }
}

/// A server trace item as this device's trace holds it.
fn trace_item(item: &TraceItemWire) -> Option<TraceItem> {
    match item {
        TraceItemWire::ToolCall {
            id,
            name,
            input,
            output,
            error,
        } => Some(TraceItem::ToolCall(ToolCallBlock {
            id: id.clone(),
            tool_name: name.clone(),
            display_name: friendly_tool_name(name),
            input: input.clone(),
            output: output.clone(),
            output_preview: None,
            state: match (error, output) {
                (Some(error), _) => ToolCallState::Error(error.clone()),
                (None, Some(_)) => ToolCallState::Success,
                // Never answered: the turn was cancelled, which is how this
                // device marks such a call too.
                (None, None) => ToolCallState::Error("Cancelled".to_string()),
            },
            duration: None,
            text_before: String::new(),
            source: ToolSource::default(),
            execution_engine: None,
        })),
        TraceItemWire::Approval {
            id,
            command,
            is_sandboxed,
            state,
            created_at,
        } => Some(TraceItem::ApprovalPrompt(ApprovalBlock {
            id: id.clone(),
            command: command.clone(),
            is_sandboxed: *is_sandboxed,
            state: match state {
                ApprovalStateWire::Approved => ApprovalState::Approved,
                // The turn is over, so an unanswered approval can never be
                // answered and its command did not run. Pending would show
                // the live prompt for it.
                ApprovalStateWire::Denied
                | ApprovalStateWire::Unanswered
                | ApprovalStateWire::Unknown => ApprovalState::Denied,
            },
            created_at: UNIX_EPOCH + Duration::from_secs(created_at.unwrap_or(0).max(0) as u64),
            asker: None,
        })),
        TraceItemWire::Unknown => None,
    }
}

// ── Hand-off (HX-13 / HX-14) ─────────────────────────────────────────────────
//
// The writes "Send to the cloud" and "Continue on this device" make, as
// HX-13 (AGE-2092) fixed them. Every reply field the client reads is
// optional, so a server a release apart still decodes.
//
// * `POST /api/conversations` with [`CreateConversation`] → the summary
//   ([`ConversationDetail`] decodes it). `client_id` makes the create
//   idempotent: an existing one answers 200 with its summary, unchanged.
// * `PUT /api/conversations/{id}/history`, body the [`ConversationData`]
//   itself, header `If-Match: <version>` → [`PlacementReply`]. 409
//   `held_elsewhere` ([`ConflictBody`]) when another device or the cloud
//   holds it (checked before the version), 412 `version_mismatch`, 428 when
//   `If-Match` is missing.
// * `POST /api/conversations/{id}/placement` with [`PlacementChange`] →
//   [`PlacementReply`]. 409 `turn_active` while a turn runs, 412
//   `version_mismatch`. Already there is a 200, so a retry is safe.
// * `GET /api/settings/models` → [`CloudModels`]: what the cloud can run.

/// The body of the idempotent create.
#[derive(Debug, serde::Serialize)]
pub struct CreateConversation<'a> {
    /// This device's id for the conversation, unique per user on the server.
    pub client_id: &'a str,
    pub title: &'a str,
    /// The model the cloud runs it on.
    pub model_id: &'a str,
    /// The full row, in stored form; its `id` is ignored.
    pub conversation: &'a ConversationData,
    /// Created held by this device, so the history and attachments are in
    /// place before the placement moves.
    pub placement: PlacementTarget,
}

/// Where a placement change moves the conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementTarget {
    Cloud,
    /// The caller's device; the server reads which one from the login
    /// session, never from the body.
    Device,
}

/// The body of a placement change. Unknown fields are a 422.
#[derive(Debug, serde::Serialize)]
pub struct PlacementChange {
    pub to: PlacementTarget,
    pub expected_version: u64,
}

/// The reply to a history push or a placement change.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PlacementReply {
    #[serde(default)]
    pub version: Option<u64>,
    #[serde(default)]
    pub placement_device: Option<PlacementDevice>,
}

/// A refusal's body (409, 412): why, in words the server wrote for the user,
/// and, for `held_elsewhere`, who holds the conversation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct ConflictBody {
    /// `held_elsewhere`, `turn_active` or `version_mismatch`.
    #[serde(default)]
    pub error: Option<String>,
    /// User-facing; shown as-is.
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub placement: Option<String>,
    #[serde(default)]
    pub placement_device: Option<PlacementDevice>,
    #[serde(default)]
    pub turn_status: Option<TurnStatus>,
    /// The version the server holds.
    #[serde(default)]
    pub version: Option<u64>,
}

impl ConflictBody {
    /// What to tell the user: the server's own message as it is, else
    /// [`continued_on`](Self::continued_on).
    pub fn notice(&self) -> String {
        match self.message.as_deref().map(str::trim) {
            Some(message) if !message.is_empty() => message.to_string(),
            _ => self.continued_on(),
        }
    }

    /// "Continued on <device>", or the generic form when the server named
    /// none.
    pub fn continued_on(&self) -> String {
        match self
            .placement_device
            .as_ref()
            .and_then(|device| device.name.as_deref())
            .filter(|name| !name.is_empty())
        {
            Some(name) => format!("Continued on {name}"),
            None => "Continued on another device".to_string(),
        }
    }

    /// Is the refusal because a turn is running?
    pub fn turn_running(&self) -> bool {
        self.error.as_deref() == Some("turn_active")
            || self
                .turn_status
                .is_some_and(|status| status != TurnStatus::Idle && status != TurnStatus::Unknown)
    }

    /// Who holds the conversation, when the refusal is `held_elsewhere`.
    pub fn holder(&self) -> Option<Holder> {
        if self.error.as_deref() != Some("held_elsewhere") {
            return None;
        }
        Some(match (self.placement.as_deref(), &self.placement_device) {
            (Some("cloud"), _) => Holder::Cloud,
            (_, device) => Holder::Device {
                device_id: device.as_ref().and_then(|device| device.id.clone()),
                name: device.as_ref().and_then(|device| device.name.clone()),
            },
        })
    }
}

/// Where a conversation is held right now, as a refusal names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Holder {
    Cloud,
    Device {
        device_id: Option<String>,
        /// The name the device signed in with, when the server keeps one.
        name: Option<String>,
    },
}

/// The reply to an accepted `PUT /api/conversations/{id}/history`: the
/// version the server now holds. The body of the push is the conversation's
/// stored form ([`ConversationData`]); [`push_body`] takes out what is
/// meaningless off this machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub struct HistoryAccepted {
    pub version: u64,
}

/// The summary a `POST /api/conversations` answers with (HX-13): the id the
/// server gave the conversation and the version it starts at.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct CreatedConversation {
    pub id: String,
    #[serde(default)]
    pub version: u64,
}

/// `row` as the server stores it: without the working directory and the sync
/// columns, which describe this device and mean nothing on the server.
pub fn push_body(mut row: ConversationData) -> ConversationData {
    row.working_dir = None;
    row.sync = None;
    row.placement = None;
    row.remote_version = None;
    row.last_synced_at = None;
    row
}

/// `GET /api/auth/sessions`: the user's login sessions. The one marked
/// `current` is this device, whose id is what a device placement names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthSessions {
    /// This device's login session id, when the server marked one current.
    pub current: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AuthSessionsWire {
    Wrapped {
        #[serde(alias = "items")]
        sessions: Vec<AuthSessionWire>,
    },
    Bare(Vec<AuthSessionWire>),
}

#[derive(Deserialize)]
struct AuthSessionWire {
    id: String,
    #[serde(default)]
    current: bool,
}

impl<'de> Deserialize<'de> for AuthSessions {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let sessions = match AuthSessionsWire::deserialize(deserializer)? {
            AuthSessionsWire::Wrapped { sessions } | AuthSessionsWire::Bare(sessions) => sessions,
        };
        Ok(AuthSessions {
            current: sessions
                .into_iter()
                .find(|session| session.current && !session.id.is_empty())
                .map(|session| session.id),
        })
    }
}

/// One model the cloud can run a conversation on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudModel {
    pub id: String,
    /// What to show; the id when the server sends no name.
    pub name: String,
}

/// The account's cloud models (`GET /api/settings/models`), and which one it
/// uses by default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CloudModels {
    pub models: Vec<CloudModel>,
    pub default_id: Option<String>,
}

impl CloudModels {
    /// Can the cloud run `model_id`? Compared by id.
    pub fn runs(&self, model_id: &str) -> bool {
        self.models.iter().any(|model| model.id == model_id)
    }

    /// The model to preselect for a conversation on `current`: `current`
    /// itself when the cloud runs it, else the account's default, else the
    /// first.
    pub fn preselected(&self, current: &str) -> Option<&CloudModel> {
        let find = |id: &str| self.models.iter().find(|model| model.id == id);
        find(current)
            .or_else(|| self.default_id.as_deref().and_then(find))
            .or_else(|| self.models.first())
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CloudModelsWire {
    Wrapped {
        #[serde(alias = "items")]
        models: Vec<CloudModelWire>,
        #[serde(default, alias = "default_model_id", alias = "default")]
        default_model: Option<String>,
    },
    Bare(Vec<CloudModelWire>),
}

#[derive(Deserialize)]
struct CloudModelWire {
    #[serde(alias = "model_id")]
    id: String,
    #[serde(default, alias = "display_name")]
    name: Option<String>,
    #[serde(default, alias = "is_default")]
    default: bool,
    #[serde(default = "enabled_by_default")]
    enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

impl<'de> Deserialize<'de> for CloudModels {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (models, default_model) = match CloudModelsWire::deserialize(deserializer)? {
            CloudModelsWire::Wrapped {
                models,
                default_model,
            } => (models, default_model),
            CloudModelsWire::Bare(models) => (models, None),
        };
        let default_id = default_model.or_else(|| {
            models
                .iter()
                .find(|model| model.default && model.enabled)
                .map(|model| model.id.clone())
        });
        Ok(CloudModels {
            models: models
                .into_iter()
                .filter(|model| model.enabled)
                .map(|model| CloudModel {
                    name: model
                        .name
                        .filter(|name| !name.trim().is_empty())
                        .unwrap_or_else(|| model.id.clone()),
                    id: model.id,
                })
                .collect(),
            default_id,
        })
    }
}

// ── Decoding ─────────────────────────────────────────────────────────────────

/// The page as it arrives: an object with the changes under `conversations`
/// (HX-1's list shape) or `changes`, or the pre-HX-1 bare array.
#[derive(Deserialize)]
#[serde(untagged)]
enum PageWire {
    Page {
        #[serde(alias = "changes", alias = "items")]
        conversations: Vec<ChangeWire>,
        #[serde(default)]
        next_cursor: Option<String>,
    },
    Bare(Vec<ChangeWire>),
}

#[derive(Deserialize)]
struct ChangeWire {
    id: String,
    #[serde(default)]
    deleted: bool,
    #[serde(default)]
    title: String,
    #[serde(default)]
    model_id: Option<String>,
    #[serde(default)]
    version: Option<u64>,
    #[serde(default)]
    updated_at_ms: Option<i64>,
    #[serde(default, deserialize_with = "unix_seconds_opt")]
    updated_at: Option<i64>,
    /// What chatty-server sends (AGE-2124): Unix seconds, in the
    /// conversation's totals. Without it every pulled row got time 0 and
    /// sorted below every local one.
    #[serde(default)]
    last_active_at: Option<i64>,
    #[serde(default)]
    turn_status: Option<TurnStatus>,
    #[serde(default)]
    turn_active: bool,
    #[serde(default, deserialize_with = "placement_opt")]
    placement: Option<Placement>,
    #[serde(default)]
    placement_device: Option<PlacementDevice>,
    /// Sent only when true.
    #[serde(default)]
    archived: bool,
}

impl<'de> Deserialize<'de> for ChangesPage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (changes, next_cursor) = match PageWire::deserialize(deserializer)? {
            PageWire::Page {
                conversations,
                next_cursor,
            } => (conversations, next_cursor),
            PageWire::Bare(changes) => (changes, None),
        };
        Ok(ChangesPage {
            changes: changes.into_iter().map(RemoteChange::from).collect(),
            next_cursor,
        })
    }
}

impl From<ChangeWire> for RemoteChange {
    fn from(wire: ChangeWire) -> Self {
        if wire.deleted {
            return RemoteChange::Deleted { id: wire.id };
        }
        let turn_status = wire.turn_status.unwrap_or(if wire.turn_active {
            TurnStatus::Running
        } else {
            TurnStatus::Idle
        });
        let model_id = wire.model_id.filter(|model| !model.is_empty());
        // A cloud placement names no model of its own; the conversation's
        // is the one its turns run on, so the composer shows that rather
        // than "Hosted model" (AGE-2129).
        let placement = match wire
            .placement
            .unwrap_or(Placement::Cloud { model_id: None })
        {
            Placement::Cloud { model_id: None } => Placement::Cloud {
                model_id: model_id.clone(),
            },
            placement => placement,
        };
        RemoteChange::Upsert(RemoteSummary {
            id: wire.id,
            title: wire.title,
            model_id,
            version: wire.version.unwrap_or(0),
            updated_at: wire
                .updated_at_ms
                .map(|ms| ms / 1_000)
                .or(wire.updated_at)
                .or(wire.last_active_at)
                .unwrap_or(0),
            turn_status,
            placement: with_device(placement, wire.placement_device.as_ref()),
            device_name: wire
                .placement_device
                .and_then(|device| device.name)
                .filter(|name| !name.is_empty()),
            archived: wire.archived,
        })
    }
}

/// The device that holds a conversation (HX-13): its login session's id and
/// the name it signed in with.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct PlacementDevice {
    /// The holder's login session (HX-7).
    #[serde(default)]
    pub id: Option<String>,
    /// What the user called that device.
    #[serde(default)]
    pub name: Option<String>,
}

/// A device placement takes its id from `placement_device` when the
/// placement itself did not carry one (HX-13 sends them side by side).
fn with_device(placement: Placement, device: Option<&PlacementDevice>) -> Placement {
    match (placement, device) {
        (Placement::Device { device_id }, Some(PlacementDevice { id: Some(id), .. }))
            if device_id.is_empty() =>
        {
            Placement::Device {
                device_id: id.clone(),
            }
        }
        (placement, _) => placement,
    }
}

/// A placement as the server sends it: `"cloud"` or `"device"` (HX-13, the
/// device named beside it in `placement_device`), or an object
/// `{"kind": "cloud" | "device", "device_id": …}`. Anything else is the
/// cloud.
fn placement_opt<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Placement>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.map(|value| {
        let device_id = value
            .get("device_id")
            .and_then(|id| id.as_str())
            .map(str::to_string);
        let kind = value
            .as_str()
            .or_else(|| value.get("kind").and_then(|kind| kind.as_str()));
        match (kind, device_id) {
            (Some("device"), device_id) => Placement::Device {
                device_id: device_id.unwrap_or_default(),
            },
            _ => Placement::Cloud {
                model_id: value
                    .get("model_id")
                    .and_then(|model| model.as_str())
                    .map(str::to_string),
            },
        }
    }))
}

/// A timestamp as unix seconds: a number of seconds, or an RFC 3339 string.
fn unix_seconds_opt<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<i64>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| match value {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => chrono::DateTime::parse_from_rfc3339(&s)
            .ok()
            .map(|at| at.timestamp()),
        _ => None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AGE-2124: chatty-server sends a conversation's time as
    /// `last_active_at` (Unix seconds), in the list and in the detail. Read
    /// as 0, every pulled conversation sorted below every local one.
    #[test]
    fn the_servers_last_active_at_is_the_time() {
        let page: ChangesPage = serde_json::from_value(serde_json::json!({
            "conversations": [{"id": "a", "title": "A", "version": 1, "last_active_at": 1_795_708_470}],
            "next_cursor": "c"
        }))
        .unwrap();
        let RemoteChange::Upsert(a) = &page.changes[0] else {
            panic!("an upsert")
        };
        assert_eq!(a.updated_at, 1_795_708_470);

        let detail: ConversationDetail = serde_json::from_value(serde_json::json!({
            "id": "a", "title": "A", "version": 1, "last_active_at": 1_795_708_470, "messages": []
        }))
        .unwrap();
        assert_eq!(
            detail.to_conversation_data("local").updated_at,
            1_795_708_470
        );
    }

    #[test]
    fn a_page_decodes_upserts_tombstones_and_the_cursor() {
        let page: ChangesPage = serde_json::from_value(serde_json::json!({
            "conversations": [
                {"id": "a", "title": "A", "version": 3, "updated_at_ms": 5_000,
                 "turn_status": "waiting_for_you", "placement": "cloud", "extra": 1},
                {"id": "b", "deleted": true},
                {"id": "c", "updated_at": "2026-11-25T10:00:00Z", "turn_status": "napping"}
            ],
            "next_cursor": "cur-2"
        }))
        .unwrap();
        assert_eq!(page.next_cursor.as_deref(), Some("cur-2"));
        assert_eq!(
            page.changes[0],
            RemoteChange::Upsert(RemoteSummary {
                id: "a".into(),
                title: "A".into(),
                model_id: None,
                version: 3,
                updated_at: 5,
                turn_status: TurnStatus::WaitingForYou,
                placement: Placement::Cloud { model_id: None },
                device_name: None,
                archived: false,
            })
        );
        assert_eq!(page.changes[1], RemoteChange::Deleted { id: "b".into() });
        let RemoteChange::Upsert(c) = &page.changes[2] else {
            panic!("c is an upsert")
        };
        assert_eq!(c.updated_at, 1_795_600_800);
        assert_eq!(c.turn_status, TurnStatus::Unknown);
    }

    /// The list before HX-1 is a bare array; it still decodes, as one page
    /// with no cursor.
    #[test]
    fn a_bare_array_decodes_as_one_page() {
        let page: ChangesPage =
            serde_json::from_str(r#"[{"id":"a","title":"A","turn_active":true}]"#).unwrap();
        assert!(page.next_cursor.is_none());
        let RemoteChange::Upsert(a) = &page.changes[0] else {
            panic!("a is an upsert")
        };
        assert_eq!(a.turn_status, TurnStatus::Running);
    }

    /// A `GET /api/conversations/{id}` body shaped exactly like
    /// chatty-server's `ConversationDetail` (hive
    /// `services/chatty-server/src/wire.rs`), totals flattened in, with the
    /// fields this client ignores (`cost_usd`, `team`, `turn_spend`,
    /// `agent_costs_usd`, `markup_usd`) present.
    fn hive_detail() -> serde_json::Value {
        use crate::services::swarm_trace::{AgentNode, AgentRecord};
        let root = AgentRecord {
            node: AgentNode::new("root", "root", None),
            parent: None,
            started_by: None,
            conversation: None,
        };
        let mut usage = TokenUsage::new(120, 40);
        usage.estimated_cost_usd = Some(0.0042);
        serde_json::json!({
            "id": "r-1",
            "title": "Cloud chat",
            "model_id": "platform-qwen3",
            "turn_active": false,
            "cost_usd": 0.0062,
            "tool_call_count": 3,
            "last_active_at": 1_795_708_470,
            "context_tokens": 2_048,
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "look"}]},
                {"role": "assistant", "content": [{"type": "text", "text": "done"}]},
                {"role": "user", "content": [{"type": "text", "text": "and?"}]},
                {"role": "assistant", "content": [{"type": "text", "text": "this"}]},
            ],
            "traces": [{
                "message_index": 1,
                "items": [
                    {"type": "tool_call", "id": "c-1", "name": "read_file",
                     "input": "{\"path\":\"a.txt\"}", "output": "hello", "error": null},
                    {"type": "tool_call", "id": "c-2", "name": "shell_execute",
                     "input": "{}", "output": null, "error": "exit 1"},
                    {"type": "tool_call", "id": "c-3", "name": "shell_execute",
                     "input": "{}", "output": null, "error": null},
                    {"type": "approval", "id": "a-1", "command": "rm x",
                     "is_sandboxed": true, "state": "approved", "created_at": 1_795_708_400},
                    {"type": "approval", "id": "a-2", "command": "rm y",
                     "is_sandboxed": false, "state": "unanswered", "created_at": null},
                    {"type": "a_kind_from_later"}
                ],
                "agents": [serde_json::to_value(&root).unwrap()],
                "agent_costs_usd": [0.002],
                "times": {"started_ms": 1_795_708_390_000_u64, "ended_ms": 1_795_708_470_500_u64,
                          "agents": [{"started_ms": 1_795_708_390_000_u64, "ended_ms": 1_795_708_470_500_u64}]}
            }, {
                // An index past the history is ignored, not a panic.
                "message_index": 9, "items": []
            }],
            "usage": [{
                "message_index": 3,
                "usage": serde_json::to_value(&usage).unwrap(),
                "markup_usd": 0.002
            }],
            "team": null,
            "turn_spend": null,
            "version": 7,
            "placement": "cloud",
            "placement_device": null,
            "turn_status": "idle"
        })
    }

    /// AGE-2157: what chatty-server sends — `traces`, `usage` and
    /// `last_active_at` — reaches the stored row.
    #[test]
    fn chatty_servers_detail_maps_traces_usage_and_times() {
        let detail: ConversationDetail = serde_json::from_value(hive_detail()).unwrap();
        assert_eq!(detail.version, Some(7));
        assert!(!detail.turn_running());
        let data = detail.to_conversation_data("local-1");

        let traces: Vec<Option<serde_json::Value>> =
            serde_json::from_str(&data.system_traces).unwrap();
        assert_eq!(traces.len(), 4, "one slot per message");
        assert!(traces[0].is_none() && traces[2].is_none() && traces[3].is_none());
        let stored = traces[1].clone().unwrap();
        assert_eq!(stored[AGENTS_TRACE_KEY][0]["name"], "root");
        let trace: SystemTrace = serde_json::from_value(stored).unwrap();
        assert_eq!(trace.items.len(), 5, "the unknown kind is left out");
        let TraceItem::ToolCall(read) = &trace.items[0] else {
            panic!("a tool call")
        };
        assert_eq!(
            (read.id.as_str(), read.tool_name.as_str()),
            ("c-1", "read_file")
        );
        assert_eq!(read.output.as_deref(), Some("hello"));
        assert_eq!(read.state, ToolCallState::Success);
        let TraceItem::ToolCall(failed) = &trace.items[1] else {
            panic!("a tool call")
        };
        assert_eq!(failed.state, ToolCallState::Error("exit 1".into()));
        let TraceItem::ToolCall(cancelled) = &trace.items[2] else {
            panic!("a tool call")
        };
        assert_eq!(cancelled.state, ToolCallState::Error("Cancelled".into()));
        let TraceItem::ApprovalPrompt(approved) = &trace.items[3] else {
            panic!("an approval")
        };
        assert_eq!(approved.state, ApprovalState::Approved);
        assert!(approved.is_sandboxed);
        assert_eq!(
            approved.created_at,
            UNIX_EPOCH + Duration::from_secs(1_795_708_400)
        );
        let TraceItem::ApprovalPrompt(unanswered) = &trace.items[4] else {
            panic!("an approval")
        };
        assert_eq!(
            unanswered.state,
            ApprovalState::Denied,
            "a finished turn's unanswered approval is never a live prompt"
        );

        let timestamps: Vec<Option<i64>> = serde_json::from_str(&data.message_timestamps).unwrap();
        assert_eq!(timestamps, vec![None, Some(1_795_708_470), None, None]);

        let usage: ConversationTokenUsage = serde_json::from_str(&data.token_usage).unwrap();
        assert_eq!(usage.message_usages.len(), 1);
        assert_eq!(
            (usage.total_input_tokens, usage.total_output_tokens),
            (120, 40)
        );

        assert_eq!(data.attachment_paths, "[]");
        assert_eq!(
            (data.created_at, data.updated_at),
            (0, 1_795_708_470),
            "no creation time is invented from the last activity"
        );
        assert_eq!((data.tool_call_count, data.context_tokens), (3, 2_048));
    }

    /// AGE-2143: paths in a detail never reach the stored row, whatever the
    /// server sends.
    #[test]
    fn a_detail_never_carries_attachment_paths() {
        let mut value = hive_detail();
        value["attachment_paths"] = serde_json::json!([["/home/u/.ssh/id_ed25519"], [], [], []]);
        let detail: ConversationDetail = serde_json::from_value(value).unwrap();
        let data = detail.to_conversation_data("local-1");
        assert_eq!(data.attachment_paths, "[]");
        assert!(!format!("{data:?}").contains("id_ed25519"));
    }

    /// A `?since=` page shaped like chatty-server's `ConversationChanges`:
    /// `ConversationSummary`s (totals flattened, `archived` only when true)
    /// and a `{id, deleted}` tombstone.
    #[test]
    fn chatty_servers_change_feed_maps_archived() {
        let summary = |id: &str, archived: Option<bool>| {
            let mut value = serde_json::json!({
                "id": id, "title": "T", "model_id": "platform-qwen3",
                "message_count": 2, "turn_active": false,
                "workspace_id": "0b5f5c9e-4f6e-4d0a-9a3b-2f1c1e0d9a7b",
                "cost_usd": null, "tool_call_count": 0,
                "last_active_at": 1_795_708_470, "context_tokens": 0,
                "version": 3, "placement": "cloud", "placement_device": null,
                "turn_status": "idle"
            });
            if let Some(archived) = archived {
                value["archived"] = archived.into();
            }
            value
        };
        let page: ChangesPage = serde_json::from_value(serde_json::json!({
            "conversations": [
                summary("a", None),
                summary("b", Some(true)),
                {"id": "c", "deleted": true}
            ],
            "next_cursor": "42"
        }))
        .unwrap();
        assert_eq!(page.next_cursor.as_deref(), Some("42"));
        let RemoteChange::Upsert(a) = &page.changes[0] else {
            panic!("an upsert")
        };
        assert!(!a.archived);
        assert_eq!(a.updated_at, 1_795_708_470);
        let RemoteChange::Upsert(b) = &page.changes[1] else {
            panic!("an upsert")
        };
        assert!(b.archived);
        assert_eq!(page.changes[2], RemoteChange::Deleted { id: "c".into() });
    }

    #[test]
    fn a_detail_without_the_new_fields_still_decodes() {
        let detail: ConversationDetail = serde_json::from_value(serde_json::json!({
            "id": "r-1", "title": "T", "model_id": "m", "turn_active": false, "messages": []
        }))
        .unwrap();
        assert!(detail.version.is_none());
        assert!(!detail.turn_running());
        let data = detail.to_conversation_data("local-1");
        assert_eq!(data.id, "local-1");
        assert_eq!(data.system_traces, "[]");
        assert_eq!(data.token_usage, "{}");

        let detail: ConversationDetail = serde_json::from_value(serde_json::json!({
            "id": "r-1", "version": 4, "placement": {"kind": "device", "device_id": "d-1"},
            "turn_status": "running", "messages": []
        }))
        .unwrap();
        assert_eq!(detail.version, Some(4));
        assert_eq!(
            detail.placement,
            Some(Placement::Device {
                device_id: "d-1".into()
            })
        );
        assert!(detail.turn_running());
    }

    #[test]
    fn the_cloud_roster_decodes_in_either_shape_and_preselects() {
        let bare: CloudModels = serde_json::from_value(serde_json::json!([
            {"id": "a", "name": "Model A"},
            {"model_id": "b", "is_default": true},
            {"id": "off", "enabled": false}
        ]))
        .unwrap();
        assert_eq!(bare.default_id.as_deref(), Some("b"));
        assert_eq!(bare.models.len(), 2);
        assert_eq!(bare.models[1].name, "b");
        assert!(bare.runs("a") && !bare.runs("off"));
        assert_eq!(bare.preselected("a").unwrap().id, "a");
        assert_eq!(bare.preselected("llama3").unwrap().id, "b");

        let wrapped: CloudModels = serde_json::from_value(serde_json::json!({
            "models": [{"id": "a"}], "default_model_id": "a"
        }))
        .unwrap();
        assert_eq!(wrapped.default_id.as_deref(), Some("a"));
    }

    #[test]
    fn a_conflict_names_its_holder_or_the_running_turn() {
        let body: ConflictBody = serde_json::from_value(serde_json::json!({
            "error": "held_elsewhere",
            "message": "This conversation is running on Work laptop. Continue it here first.",
            "placement": "device",
            "placement_device": {"id": "s-2", "name": "Work laptop"},
            "version": 4
        }))
        .unwrap();
        assert!(!body.turn_running());
        assert_eq!(
            body.holder(),
            Some(Holder::Device {
                device_id: Some("s-2".into()),
                name: Some("Work laptop".into())
            })
        );
        let body: ConflictBody = serde_json::from_value(serde_json::json!({
            "error": "turn_active", "message": "A turn is running.", "turn_status": "running"
        }))
        .unwrap();
        assert!(body.turn_running());
        assert_eq!(body.holder(), None);
        let body: ConflictBody = serde_json::from_value(serde_json::json!({
            "error": "held_elsewhere", "placement": "cloud", "placement_device": null
        }))
        .unwrap();
        assert_eq!(body.holder(), Some(Holder::Cloud));
    }

    #[test]
    fn the_current_login_session_is_this_device() {
        let sessions: AuthSessions = serde_json::from_value(serde_json::json!([
            {"id": "s-1", "current": false, "name": "Laptop"},
            {"id": "s-2", "current": true}
        ]))
        .unwrap();
        assert_eq!(sessions.current.as_deref(), Some("s-2"));
        let none: AuthSessions =
            serde_json::from_value(serde_json::json!({"sessions": [{"id": "s-1"}]})).unwrap();
        assert_eq!(none.current, None);
    }

    /// HX-13 sends the placement as a bare word with the holding device
    /// beside it; a device placement must not read as the cloud.
    #[test]
    fn a_device_placement_takes_its_id_from_placement_device() {
        let page: ChangesPage = serde_json::from_value(serde_json::json!({
            "conversations": [{"id": "a", "version": 2, "placement": "device",
                               "placement_device": {"id": "s-1", "name": "Mac"}}],
            "next_cursor": "c"
        }))
        .unwrap();
        let RemoteChange::Upsert(a) = &page.changes[0] else {
            panic!("a is an upsert")
        };
        assert_eq!(
            a.placement,
            Placement::Device {
                device_id: "s-1".into()
            }
        );
        assert_eq!(a.device_name.as_deref(), Some("Mac"));
        let detail: ConversationDetail = serde_json::from_value(serde_json::json!({
            "id": "a", "placement": "device", "placement_device": {"id": "s-1", "name": null}
        }))
        .unwrap();
        assert_eq!(
            detail.current_placement(),
            Some(Placement::Device {
                device_id: "s-1".into()
            })
        );
    }

    #[test]
    fn a_push_leaves_this_devices_paths_and_sync_state_behind() {
        let mut row = ConversationDetail::default().to_conversation_data("local");
        row.working_dir = Some("/home/me/project".into());
        row.sync = Some("{}".into());
        row.placement = Some("{}".into());
        row.remote_version = Some(3);
        row.last_synced_at = Some(9);
        let body = push_body(row);
        assert!(body.working_dir.is_none() && body.sync.is_none() && body.placement.is_none());
        assert!(body.remote_version.is_none() && body.last_synced_at.is_none());
    }
}
