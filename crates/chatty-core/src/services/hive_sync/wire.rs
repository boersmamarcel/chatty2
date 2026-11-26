//! Every request and response shape of chatty-server's conversation sync API,
//! in one place (AGE-2088).
//!
//! Written against the shape HX-8 (AGE-2087) documents, before that issue
//! merged: when it lands, this file is the one to adjust. Decoding is
//! deliberately tolerant — unknown fields are ignored, and fields HX-8 adds
//! are optional — so a server a release behind or ahead still decodes.
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
//! * `GET /api/conversations/{id}` → [`ConversationDetail`], which adds
//!   `version`, `placement` and `turn_status` to the conversation.

use std::path::PathBuf;

use rig_core::completion::Message;
use serde::{Deserialize, Deserializer};

use crate::models::conversation::Placement;
use crate::repositories::ConversationData;

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
}

/// `GET /api/conversations/{id}`: the conversation, and as much of the
/// server's stored row as it sends.
///
/// The history is required; the per-message metadata and totals are
/// optional, so a server that does not send them yet decodes, and
/// [`Conversation::replace_from_remote`](crate::models::conversation::Conversation::replace_from_remote)
/// keeps this device's copy of whatever is absent.
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
    /// The holding device (HX-13), when the conversation runs on one.
    #[serde(default)]
    pub placement_device: Option<PlacementDevice>,
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub system_traces: Option<Vec<Option<serde_json::Value>>>,
    #[serde(default)]
    pub attachment_paths: Option<Vec<Vec<PathBuf>>>,
    #[serde(default)]
    pub message_timestamps: Option<Vec<Option<i64>>>,
    #[serde(default)]
    pub message_feedback: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub token_usage: Option<serde_json::Value>,
    #[serde(default)]
    pub tool_call_count: Option<u32>,
    #[serde(default)]
    pub context_tokens: Option<u32>,
    #[serde(default, deserialize_with = "unix_seconds_opt")]
    pub created_at: Option<i64>,
    #[serde(default, deserialize_with = "unix_seconds_opt")]
    pub updated_at: Option<i64>,
}

impl ConversationDetail {
    /// Where this conversation runs: the holding device when the server
    /// names one, else its `placement`.
    pub fn effective_placement(&self) -> Option<Placement> {
        placement_of(self.placement.clone(), self.placement_device.as_ref())
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
    /// The sync columns are left empty: the caller sets the sync state.
    pub fn to_conversation_data(&self, local_id: &str) -> ConversationData {
        fn json<T: serde::Serialize>(value: &Option<T>, empty: &str) -> String {
            value
                .as_ref()
                .and_then(|value| serde_json::to_string(value).ok())
                .unwrap_or_else(|| empty.to_string())
        }
        ConversationData {
            id: local_id.to_string(),
            title: self.title.clone(),
            model_id: self.model_id.clone(),
            message_history: serde_json::to_string(&self.messages)
                .unwrap_or_else(|_| "[]".to_string()),
            system_traces: json(&self.system_traces, "[]"),
            token_usage: json(&self.token_usage, "{}"),
            attachment_paths: json(&self.attachment_paths, "[]"),
            message_timestamps: json(&self.message_timestamps, "[]"),
            message_feedback: json(&self.message_feedback, "[]"),
            regeneration_records: "[]".to_string(),
            created_at: self.created_at.unwrap_or(0),
            updated_at: self.updated_at.unwrap_or(0),
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
}

// ── Writes (HX-13, AGE-2092; AGE-2096) ───────────────────────────────────────

/// The device that holds a conversation, as the server names it: the id is
/// the holder's login session (HX-7), and the name is what the user called
/// that device. `null` on the wire when the conversation runs in the cloud.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct PlacementDevice {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

/// The placement a row has: the holding device when the server names one,
/// else what `placement` says.
fn placement_of(
    placement: Option<Placement>,
    device: Option<&PlacementDevice>,
) -> Option<Placement> {
    match device.and_then(|device| device.id.clone()) {
        Some(device_id) => Some(Placement::Device { device_id }),
        None => placement,
    }
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

/// The body of a 409 `held_elsewhere`: who holds the conversation now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct HeldBy {
    #[serde(default)]
    pub placement_device: Option<PlacementDevice>,
    #[serde(default)]
    pub message: Option<String>,
    /// The version the server holds.
    #[serde(default)]
    pub version: Option<u64>,
}

impl HeldBy {
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
    #[serde(default)]
    turn_status: Option<TurnStatus>,
    #[serde(default)]
    turn_active: bool,
    #[serde(default, deserialize_with = "placement_opt")]
    placement: Option<Placement>,
    #[serde(default)]
    placement_device: Option<PlacementDevice>,
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
        RemoteChange::Upsert(RemoteSummary {
            id: wire.id,
            title: wire.title,
            model_id: wire.model_id.filter(|model| !model.is_empty()),
            version: wire.version.unwrap_or(0),
            updated_at: wire
                .updated_at_ms
                .map(|ms| ms / 1_000)
                .or(wire.updated_at)
                .unwrap_or(0),
            turn_status,
            placement: placement_of(wire.placement, wire.placement_device.as_ref())
                .unwrap_or(Placement::Cloud { model_id: None }),
        })
    }
}

/// A placement as the server sends it: `"cloud"`, or an object
/// `{"kind": "cloud" | "device", "device_id": …}`. Anything else is the
/// cloud, the only placement the server has until HX-13.
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
            (Some("device"), Some(device_id)) => Placement::Device { device_id },
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

    /// HX-13: a device placement arrives as `"placement": "device"` plus the
    /// holder in `placement_device`, on a list row and on the detail alike.
    #[test]
    fn a_device_placement_is_read_from_placement_device() {
        let page: ChangesPage = serde_json::from_value(serde_json::json!({
            "conversations": [
                {"id": "r-1", "version": 5, "placement": "device",
                 "placement_device": {"id": "s-2", "name": "Work laptop"}},
                {"id": "r-2", "version": 1, "placement": "cloud", "placement_device": null}
            ],
            "next_cursor": "c"
        }))
        .unwrap();
        let placements: Vec<_> = page
            .changes
            .iter()
            .map(|change| match change {
                RemoteChange::Upsert(summary) => summary.placement.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            placements,
            vec![
                Placement::Device {
                    device_id: "s-2".into()
                },
                Placement::Cloud { model_id: None }
            ]
        );

        let detail: ConversationDetail = serde_json::from_value(serde_json::json!({
            "id": "r-1", "placement": "device",
            "placement_device": {"id": "s-2", "name": null}
        }))
        .unwrap();
        assert_eq!(
            detail.effective_placement(),
            Some(Placement::Device {
                device_id: "s-2".into()
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
