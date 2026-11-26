//! Handing one conversation between this device and the cloud (HX-14,
//! AGE-2093; replaces AGE-298's take-online / bring-back).
//!
//! * [`send_to_cloud`]: create the conversation on the server (idempotent by
//!   `client_id`) or push this device's history to it (`If-Match`), upload its
//!   attachments, then move its placement to the cloud.
//! * [`continue_here`]: pull the server's copy, then take the placement for
//!   this device.
//!
//! Both return the [`ConversationSync`] the conversation should now have, and
//! change nothing local themselves. That order is the whole safety property:
//! the caller records the new state only after every step succeeded, so a
//! failure or a client killed part-way leaves the local copy exactly as it
//! was. A retry after a failure re-runs the steps; the create's `client_id`
//! makes the server answer with the conversation the first attempt made
//! rather than a second copy.
//!
//! The wire shapes live in [`crate::services::hive_sync::wire`].
//!
//! # What never moves
//!
//! Files in the working folder, local MCP servers, provider keys, memory and
//! skills (plan D7). The row sent up has its working folder and this device's
//! sync columns cleared; provider keys never leave this process at all. The
//! frontends show [`SEND_TO_CLOUD_SUMMARY`] before the user confirms.

use std::sync::Arc;

use hive_client::HiveSession;
use reqwest::StatusCode;

use super::hosted::{SIGN_IN_TO_HIVE, send_to_server};
use crate::models::conversation::{ConversationSync, Placement, SyncedConversation};
use crate::repositories::ConversationData;
use crate::repositories::conversation_repository::BoxFuture;
use crate::services::hive_sync::wire::{
    AuthSessions, CloudModels, ConflictBody, ConversationDetail, CreateConversation, Holder,
    PlacementChange, PlacementReply, PlacementTarget, push_body,
};

// ── What the user is told ────────────────────────────────────────────────────

/// What a hand-off takes along and leaves behind, for the confirmation the
/// user sees. Data, not layout, so the desktop dialog and the TUI say the
/// same thing (HX-16).
pub struct HandoffSummary {
    /// One sentence on what happens.
    pub lead: &'static str,
    /// The heading over [`goes`](Self::goes).
    pub goes_title: &'static str,
    /// What the hand-off carries.
    pub goes: &'static [&'static str],
    /// The heading over [`stays`](Self::stays).
    pub stays_title: &'static str,
    /// What it leaves where it is.
    pub stays: &'static [&'static str],
}

/// "Send to the cloud".
pub const SEND_TO_CLOUD_SUMMARY: HandoffSummary = HandoffSummary {
    lead: "The conversation keeps running in your Hive account, even after you close this \
           computer. You can open it from any device you are signed in on.",
    goes_title: "Goes with it",
    goes: &["Messages", "Tool activity", "Usage", "Attachments"],
    stays_title: "Stays on this computer",
    stays: &[
        "Files in your working folder",
        "Local MCP servers",
        "Your provider keys",
        "Memory",
        "Skills",
    ],
};

/// What the dialog says about the model when the conversation's own model
/// cannot run in the cloud.
pub const LOCAL_MODEL_NOTE: &str = "This conversation uses a model that only runs on this computer. In the cloud it uses \
     the model you pick here.";

/// "Continue on this device".
pub const CONTINUE_HERE_SUMMARY: HandoffSummary = HandoffSummary {
    lead: "The next reply runs on this computer, with its own models and files. The cloud \
           copy stays in your Hive account.",
    goes_title: "Comes to this computer",
    goes: &["The latest messages", "Tool activity", "Usage"],
    stays_title: "Stays in your Hive account",
    stays: &[
        "The cloud copy of the conversation",
        "Files the cloud run saved",
    ],
};

// ── Errors ───────────────────────────────────────────────────────────────────

/// Why a hand-off did not happen. Every message is written for the user and
/// names the next step; nothing local changed in any of these cases.
#[derive(Debug, thiserror::Error)]
pub enum HandoffError {
    /// A reply is still being written, here or in the cloud.
    #[error("A reply is still being written. Wait for it to finish, or stop it, then try again.")]
    TurnRunning,
    /// Another device, or the cloud, holds the conversation: only it may
    /// change it. [`continue_here`] takes it over. `message` is the
    /// server's own user-facing text, shown as-is when it sent one.
    #[error("{}", message.clone().unwrap_or_else(|| held_elsewhere(holder)))]
    HeldElsewhere {
        holder: Holder,
        message: Option<String>,
    },
    /// The server's copy changed since this device last saw it (412).
    #[error(
        "This conversation changed on another device. Continue it on this device to get the \
         latest copy, then send it again."
    )]
    ChangedElsewhere,
    /// The conversation's model does not run in the cloud and no cloud model
    /// was picked (or the one picked is not offered). The roster is the
    /// choice to show.
    #[error("Pick a cloud model for this conversation, then send it again.")]
    NeedsCloudModel(CloudModels),
    /// It already runs where it was asked to go.
    #[error("{}", if *in_cloud {
        "This conversation already runs in the cloud."
    } else {
        "This conversation already runs on this device."
    })]
    AlreadyThere { in_cloud: bool },
    /// Only on this computer: there is no cloud copy to continue from.
    #[error("This conversation is only on this computer. Send it to the cloud first.")]
    NotSynced,
    /// 401: the Hive sign-in is missing or no longer accepted.
    #[error("{SIGN_IN_TO_HIVE}")]
    SignedOut,
    /// The conversation is gone from the account.
    #[error("This conversation is no longer in your Hive account.")]
    NotFound,
    /// 410 on the create: this conversation was deleted on the account, and
    /// its id stays retired (the tombstone lasts 30 days). The caller makes
    /// it local-only and keeps the local copy. `message` is the server's own.
    #[error("{}", message.as_deref().unwrap_or("This conversation was deleted from your Hive account. It stays on this computer."))]
    DeletedOnAccount { message: Option<String> },
    /// The attachment upload failed.
    #[error("Could not upload this conversation's attachments: {0:#}. Try again.")]
    Attachments(anyhow::Error),
    /// Anything else: unreachable, refused, or a reply this build cannot read.
    #[error("Could not reach your Hive account: {0:#}. Check your connection and try again.")]
    Server(anyhow::Error),
}

fn held_elsewhere(holder: &Holder) -> String {
    match holder {
        Holder::Cloud => "This conversation is running in the cloud. Continue it on this \
                          device to take it over."
            .to_string(),
        Holder::Device { name, .. } => format!(
            "This conversation is open on {}, the only place it can change right now. \
             Continue it on this device to take it over.",
            name.as_deref().unwrap_or("another device")
        ),
    }
}

/// Why a hand-off cannot start right now, before any request is made.
pub fn refuse_reason(
    turn_active: bool,
    sync: &ConversationSync,
    to_cloud: bool,
) -> Option<HandoffError> {
    if turn_active {
        // A turn can be blocked on an approval or a clarification that lives
        // in the stores of the session that raised it.
        return Some(HandoffError::TurnRunning);
    }
    if !to_cloud && sync.synced().is_none() {
        return Some(HandoffError::NotSynced);
    }
    (sync.runs_in_cloud() == to_cloud).then_some(HandoffError::AlreadyThere { in_cloud: to_cloud })
}

// ── The server ───────────────────────────────────────────────────────────────

/// The Hive server a hand-off talks to, signed in as the user.
#[derive(Clone)]
pub struct HandoffServer {
    http: reqwest::Client,
    url: String,
    auth: Option<Arc<HiveSession>>,
}

impl HandoffServer {
    /// `url` is the account's Hive URL (D5). `auth` is the Hive sign-in;
    /// `None` sends the requests bare, which a server with auth on refuses.
    pub fn new(url: &str, auth: Option<Arc<HiveSession>>) -> Self {
        Self {
            http: reqwest::Client::new(),
            url: url.trim_end_matches('/').to_string(),
            auth,
        }
    }

    /// The base URL, without a trailing slash.
    pub fn url(&self) -> &str {
        &self.url
    }

    async fn send(
        &self,
        url: &str,
        build: impl Fn(&reqwest::Client) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, HandoffError> {
        let response = send_to_server(self.auth.as_deref(), url, || build(&self.http))
            .await
            .map_err(HandoffError::Server)?;
        match response.status() {
            StatusCode::UNAUTHORIZED => Err(HandoffError::SignedOut),
            StatusCode::NOT_FOUND => Err(HandoffError::NotFound),
            StatusCode::PRECONDITION_FAILED => Err(HandoffError::ChangedElsewhere),
            StatusCode::GONE => {
                let body: ConflictBody = response.json().await.unwrap_or_default();
                Err(HandoffError::DeletedOnAccount {
                    message: body.message.filter(|message| !message.trim().is_empty()),
                })
            }
            StatusCode::CONFLICT => {
                let body: ConflictBody = response.json().await.unwrap_or_default();
                Err(match body.holder() {
                    Some(holder) => HandoffError::HeldElsewhere {
                        holder,
                        message: body.message,
                    },
                    None => HandoffError::TurnRunning,
                })
            }
            status if !status.is_success() => {
                let detail = response.text().await.unwrap_or_default();
                Err(HandoffError::Server(anyhow::anyhow!(
                    "the server answered {status}: {}",
                    detail.trim()
                )))
            }
            _ => Ok(response),
        }
    }

    async fn json<T: serde::de::DeserializeOwned>(
        response: reqwest::Response,
    ) -> Result<T, HandoffError> {
        response
            .json()
            .await
            .map_err(|error| HandoffError::Server(anyhow::anyhow!("unexpected reply: {error}")))
    }
}

/// The models the account can run in the cloud, and its default.
pub async fn cloud_models(server: &HandoffServer) -> Result<CloudModels, HandoffError> {
    let url = format!("{}/api/settings/models", server.url);
    let response = server.send(&url, |http| http.get(&url)).await?;
    HandoffServer::json(response).await
}

/// This device's id as the server knows it: the caller's login session, the
/// row of `GET /api/auth/sessions` marked `current`. A device placement names
/// it. Cache it per sign-in; it never goes in a request body.
pub async fn this_device_id(server: &HandoffServer) -> Result<String, HandoffError> {
    let url = format!("{}/api/auth/sessions", server.url);
    let response = server.send(&url, |http| http.get(&url)).await?;
    let sessions: AuthSessions = HandoffServer::json(response).await?;
    sessions.current.ok_or_else(|| {
        HandoffError::Server(anyhow::anyhow!(
            "the server did not say which sign-in is this device's"
        ))
    })
}

/// The server's copy of a synced conversation.
pub async fn fetch_conversation(
    server: &HandoffServer,
    remote_id: &str,
) -> Result<ConversationDetail, HandoffError> {
    let url = format!("{}/api/conversations/{remote_id}", server.url);
    let response = server.send(&url, |http| http.get(&url)).await?;
    HandoffServer::json(response).await
}

// ── Attachments (HX-15) ──────────────────────────────────────────────────────

/// Uploads a conversation's attachments through the Hive files API (HX-15)
/// and returns the row with its attachment references pointing at the
/// uploads. Runs after the conversation exists on the server and before its
/// placement moves; an error stops the hand-off with nothing local changed.
pub trait AttachmentUploader: Send + Sync {
    fn upload_attachments<'a>(
        &'a self,
        server: &'a HandoffServer,
        remote_id: &'a str,
        conversation: ConversationData,
    ) -> BoxFuture<'a, anyhow::Result<ConversationData>>;
}

/// Uploads nothing: the row goes up with the references it has.
pub struct NoAttachmentUpload;

impl AttachmentUploader for NoAttachmentUpload {
    fn upload_attachments<'a>(
        &'a self,
        _server: &'a HandoffServer,
        _remote_id: &'a str,
        conversation: ConversationData,
    ) -> BoxFuture<'a, anyhow::Result<ConversationData>> {
        Box::pin(async move { Ok(conversation) })
    }
}

// ── Send to the cloud ────────────────────────────────────────────────────────

/// What [`send_to_cloud`] sends.
pub struct SendToCloud<'a> {
    /// The full row, as this device stores it ([`ConversationSnapshot::to_data`](crate::models::conversation::ConversationSnapshot::to_data)).
    /// Its `id` is the `client_id` of the create.
    pub conversation: &'a ConversationData,
    /// The conversation's sync state now.
    pub sync: &'a ConversationSync,
    /// The cloud model the user picked; `None` keeps the conversation's own.
    pub cloud_model: Option<&'a str>,
}

/// Send a conversation to the cloud and return the sync state that now
/// describes it. Nothing local changes here; the caller records the result.
///
/// Steps, each refusing with nothing local changed:
/// 1. Check the model: the conversation's own, or `cloud_model`, must be one
///    the cloud runs, else [`HandoffError::NeedsCloudModel`] with the roster.
/// 2. Not synced yet: create it with `client_id` and the full row. Synced:
///    push the history with `If-Match: <version>`.
/// 3. Upload its attachments, pushing the history again when that rewrote
///    references.
/// 4. Move the placement to the cloud.
pub async fn send_to_cloud(
    server: &HandoffServer,
    request: SendToCloud<'_>,
    uploader: &dyn AttachmentUploader,
) -> Result<ConversationSync, HandoffError> {
    if request.sync.runs_in_cloud() {
        return Ok(request.sync.clone());
    }

    // PHASE 1: the model.
    let roster = cloud_models(server).await?;
    let model_id = request
        .cloud_model
        .unwrap_or(&request.conversation.model_id)
        .to_string();
    if !roster.runs(&model_id) {
        return Err(HandoffError::NeedsCloudModel(roster));
    }
    let row = cloud_copy(request.conversation, &model_id);

    // PHASE 2: get the history onto the server.
    let (remote_id, mut version) = match request.sync.synced() {
        None => create(server, &row).await?,
        Some(synced) => {
            let version = push_history(server, &synced.remote_id, synced.version, &row).await?;
            (synced.remote_id.clone(), version)
        }
    };

    // PHASE 3: attachments.
    let uploaded = uploader
        .upload_attachments(server, &remote_id, row.clone())
        .await
        .map_err(HandoffError::Attachments)?;
    if uploaded.attachment_paths != row.attachment_paths {
        version = push_history(server, &remote_id, version, &uploaded).await?;
    }

    // PHASE 4: the placement. The model went up with the history.
    let reply = change_placement(server, &remote_id, PlacementTarget::Cloud, version).await?;

    Ok(ConversationSync::Synced(SyncedConversation {
        server_url: server.url.clone(),
        remote_id,
        version: reply.version.unwrap_or(version),
        placement: Placement::Cloud {
            model_id: Some(model_id),
        },
        last_synced_at: Some(now_ms()),
    }))
}

/// The row as the server gets it: the cloud model, and nothing that only
/// means something on this device.
fn cloud_copy(conversation: &ConversationData, model_id: &str) -> ConversationData {
    ConversationData {
        model_id: model_id.to_string(),
        ..push_body(conversation.clone())
    }
}

async fn create(
    server: &HandoffServer,
    row: &ConversationData,
) -> Result<(String, u64), HandoffError> {
    let url = format!("{}/api/conversations", server.url);
    let body = CreateConversation {
        client_id: &row.id,
        title: &row.title,
        model_id: &row.model_id,
        conversation: row,
        placement: PlacementTarget::Device,
    };
    let response = server
        .send(&url, |http| http.post(&url).json(&body))
        .await?;
    let created: ConversationDetail = HandoffServer::json(response).await?;
    Ok((created.id, created.version.unwrap_or(0)))
}

async fn push_history(
    server: &HandoffServer,
    remote_id: &str,
    version: u64,
    row: &ConversationData,
) -> Result<u64, HandoffError> {
    let url = format!("{}/api/conversations/{remote_id}/history", server.url);
    let response = server
        .send(&url, |http| {
            http.put(&url)
                .header(reqwest::header::IF_MATCH, version.to_string())
                .json(row)
        })
        .await?;
    let reply: PlacementReply = response.json().await.unwrap_or_default();
    Ok(reply.version.unwrap_or(version + 1))
}

async fn change_placement(
    server: &HandoffServer,
    remote_id: &str,
    to: PlacementTarget,
    expected_version: u64,
) -> Result<PlacementReply, HandoffError> {
    let url = format!("{}/api/conversations/{remote_id}/placement", server.url);
    let body = PlacementChange {
        to,
        expected_version,
    };
    let response = server
        .send(&url, |http| http.post(&url).json(&body))
        .await?;
    Ok(response.json().await.unwrap_or_default())
}

// ── Continue on this device ──────────────────────────────────────────────────

/// What [`continue_here`] brought back.
#[derive(Debug)]
pub struct ContinuedHere {
    /// The server's copy, to reconcile the local one against
    /// ([`Conversation::replace_from_remote`](crate::models::conversation::Conversation::replace_from_remote)).
    pub remote: ConversationDetail,
    /// The sync state the conversation now has: placed on this device.
    pub sync: ConversationSync,
}

/// Pull a synced conversation's latest copy and take its placement for this
/// device. Nothing local changes here; the caller adopts the copy and records
/// the state. Also how a conversation held by another device is taken over.
///
/// `device_id` is this device's, from [`this_device_id`] (cached per
/// sign-in by the caller); the placement records it.
pub async fn continue_here(
    server: &HandoffServer,
    sync: &ConversationSync,
    device_id: &str,
) -> Result<ContinuedHere, HandoffError> {
    if device_id.is_empty() {
        return Err(HandoffError::Server(anyhow::anyhow!(
            "this device's sign-in id is unknown"
        )));
    }
    let Some(synced) = sync.synced() else {
        return Err(HandoffError::NotSynced);
    };
    // A save between the pull and the move (a rename, the cloud's last
    // write) raises the version: pull again and retry once.
    let mut retried = false;
    let (remote, version, reply) = loop {
        let remote = fetch_conversation(server, &synced.remote_id).await?;
        if remote.turn_running() {
            return Err(HandoffError::TurnRunning);
        }
        let version = remote.version.unwrap_or(synced.version);
        match change_placement(server, &synced.remote_id, PlacementTarget::Device, version).await {
            Ok(reply) => break (remote, version, reply),
            Err(HandoffError::ChangedElsewhere) if !retried => retried = true,
            Err(error) => return Err(error),
        }
    };
    let sync = ConversationSync::Synced(SyncedConversation {
        server_url: server.url.clone(),
        remote_id: synced.remote_id.clone(),
        version: reply.version.unwrap_or(version),
        placement: Placement::Device {
            device_id: device_id.to_string(),
        },
        last_synced_at: Some(now_ms()),
    });
    Ok(ContinuedHere { remote, sync })
}

/// The local model to continue on: the cloud's when it is configured here,
/// else the conversation's own when it is, else `fallback` (the app's
/// default). `None` when none of them is configured.
pub fn pick_local_model(
    cloud_model: &str,
    current: &str,
    fallback: Option<&str>,
    configured: impl Fn(&str) -> bool,
) -> Option<String> {
    [Some(cloud_model), Some(current), fallback]
        .into_iter()
        .flatten()
        .find(|model| !model.is_empty() && configured(model))
        .map(str::to_string)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests;
