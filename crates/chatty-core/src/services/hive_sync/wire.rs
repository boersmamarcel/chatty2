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
    /// Is a turn running on the server, by either the HX-8 status or the
    /// older flag?
    pub fn turn_running(&self) -> bool {
        match self.turn_status {
            Some(status) => status != TurnStatus::Idle && status != TurnStatus::Unknown,
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

// ── Hand-off (HX-13 / HX-14) ─────────────────────────────────────────────────
//
// The writes "Send to the cloud" and "Continue on this device" make, written
// against HX-13's (AGE-2092) spec before its exact JSON was fixed. Every
// field the client reads is optional, so adjusting to the final shape is a
// change here and nowhere else.
//
// * `POST /api/conversations` with [`CreateConversation`] → a
//   [`ConversationDetail`]. `client_id` makes the create idempotent: a retry
//   returns the conversation the first attempt made.
// * `PUT /api/conversations/{id}/history` with `If-Match: <version>` and
//   [`PushHistory`] → [`VersionReply`]. 412 for a stale version, 409 (with a
//   [`ConflictBody`]) when another device or the cloud holds it.
// * `POST /api/conversations/{id}/placement` with [`PlacementChange`] →
//   [`PlacementReply`]. 409 while a turn runs.
// * `GET /api/settings/models` → [`CloudModels`]: what the cloud can run.

/// The body of the idempotent create.
#[derive(Debug, serde::Serialize)]
pub struct CreateConversation<'a> {
    /// This device's id for the conversation, unique per user on the server.
    pub client_id: &'a str,
    pub title: &'a str,
    pub model_id: &'a str,
    /// The history as messages too, for a server that reads only these.
    pub messages: serde_json::Value,
    /// The full row: traces, usage, timestamps, attachment refs.
    pub conversation: &'a ConversationData,
}

/// The body of a history push.
#[derive(Debug, serde::Serialize)]
pub struct PushHistory<'a> {
    pub conversation: &'a ConversationData,
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

/// The body of a placement change.
#[derive(Debug, serde::Serialize)]
pub struct PlacementChange<'a> {
    pub to: PlacementTarget,
    pub expected_version: u64,
    /// The model the cloud runs it on; sent only for `to: cloud`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<&'a str>,
}

/// A reply that names the conversation's new version.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct VersionReply {
    #[serde(default)]
    pub version: Option<u64>,
}

/// The reply to a placement change.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PlacementReply {
    #[serde(default)]
    pub version: Option<u64>,
    /// As sent; read with [`PlacementReply::device_id`].
    #[serde(default)]
    pub placement: Option<serde_json::Value>,
    #[serde(default)]
    pub device_id: Option<String>,
}

impl PlacementReply {
    /// The device the conversation is now placed on, when the server says.
    pub fn device_id(&self) -> Option<String> {
        self.placement
            .as_ref()
            .and_then(|placement| placement.get("device_id"))
            .and_then(|id| id.as_str())
            .map(str::to_string)
            .or_else(|| self.device_id.clone())
    }
}

/// A 409's body: why the write was refused and, when another device or the
/// cloud holds the conversation, which one.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ConflictBody {
    /// A machine-readable reason, e.g. `turn_active` or `not_holder`.
    #[serde(default, alias = "code")]
    pub error: Option<String>,
    #[serde(default, alias = "held_by", alias = "placement")]
    pub holder: Option<serde_json::Value>,
    #[serde(default)]
    pub turn_status: Option<TurnStatus>,
}

impl ConflictBody {
    /// Is the refusal because a turn is running?
    pub fn turn_running(&self) -> bool {
        let reason = self.error.as_deref().unwrap_or_default();
        reason.contains("turn")
            || self
                .turn_status
                .is_some_and(|status| status != TurnStatus::Idle && status != TurnStatus::Unknown)
    }

    /// Who holds the conversation, when the body says.
    pub fn holder(&self) -> Option<Holder> {
        let value = self.holder.as_ref()?;
        let field = |name: &str| {
            value
                .get(name)
                .and_then(|field| field.as_str())
                .filter(|field| !field.is_empty())
                .map(str::to_string)
        };
        let kind = value.as_str().map(str::to_string).or_else(|| field("kind"));
        match kind.as_deref() {
            Some("cloud") => Some(Holder::Cloud),
            _ => Some(Holder::Device {
                device_id: field("device_id"),
                name: field("device_name").or_else(|| field("name")),
            }),
        }
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
    #[serde(default)]
    turn_status: Option<TurnStatus>,
    #[serde(default)]
    turn_active: bool,
    #[serde(default, deserialize_with = "placement_opt")]
    placement: Option<Placement>,
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
            placement: wire
                .placement
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
            "error": "not_holder",
            "holder": {"kind": "device", "device_id": "d-2", "device_name": "Laptop"}
        }))
        .unwrap();
        assert!(!body.turn_running());
        assert_eq!(
            body.holder(),
            Some(Holder::Device {
                device_id: Some("d-2".into()),
                name: Some("Laptop".into())
            })
        );
        let body: ConflictBody =
            serde_json::from_value(serde_json::json!({"error": "turn_active"})).unwrap();
        assert!(body.turn_running());
        assert_eq!(body.holder(), None);
        let body: ConflictBody =
            serde_json::from_value(serde_json::json!({"held_by": "cloud"})).unwrap();
        assert_eq!(body.holder(), Some(Holder::Cloud));
    }
}
