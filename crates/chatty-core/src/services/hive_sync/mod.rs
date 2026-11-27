//! Syncing the conversation list with the user's Hive account (AGE-2088,
//! plan "Chatty × Hive sync", D1–D3).
//!
//! The server is canonical for a synced conversation and this device's
//! SQLite is a cache of it (D3). A *pull* asks the server what changed since
//! the last one and brings the local rows in line:
//!
//! * [`wire`] — the API's request and response shapes, in one place;
//! * [`HiveConversationsApi`] — the two reads, over HTTP
//!   ([`HttpHiveConversationsApi`]) or scripted ([`FakeHiveConversations`]);
//! * [`SyncEngine`] — pure: local rows and a page in, [`SyncAction`]s out;
//! * [`pull`] — drives the engine page by page, restarting on 410;
//! * [`apply_to_repository`] / [`apply_to_rows`] — carry the actions out on
//!   the persisted rows and the sidebar's metadata.
//!
//! A frontend pulls while signed in: at start, on window focus and wake, and
//! every [`PULL_INTERVAL`] while the app is open. There is no background
//! process. Pushing this device's turns is [`push`](PushQueue) (HX-17); a pull
//! never writes to the server.

mod api;
mod engine;
#[cfg(any(test, feature = "test-support"))]
mod fake;
mod notify;
mod push;
pub mod wire;

use std::time::Duration;

pub use api::{ApiError, HiveConversationsApi, HttpHiveConversationsApi, MAX_CONVERSATION_BYTES};
pub(crate) use api::{limit_text, read_json_capped};
pub use engine::{PageOutcome, RemoteRow, SyncAction, SyncEngine, apply_to_rows};
#[cfg(any(test, feature = "test-support"))]
pub use fake::FakeHiveConversations;
pub use notify::{CloudNotice, NotifyMemory, TransitionDetector};
pub use push::{
    HiveHistoryApi, HttpHiveHistoryApi, PushError, PushEvent, PushJob, PushQueue, PushTarget,
    TOO_LARGE_TO_SYNC, drain, sync_after_stale, sync_after_turn,
};
pub use wire::{
    AuthSessions, ChangesPage, CloudModel, CloudModels, ConflictBody, ConversationDetail,
    CreatedConversation, FULL_SYNC_CURSOR, HistoryAccepted, Holder, PAGE_LIMIT, PlacementDevice,
    RemoteChange, RemoteSummary, TurnStatus, push_body,
};

use crate::models::conversation::ConversationSync;
use crate::repositories::error::RepositoryResult;
use crate::repositories::{ConversationData, ConversationMetadata, ConversationRepository};

/// What a frontend tells the user when a cloud conversation could not be
/// brought up to date (HX-10), the same words on every surface.
pub const COULD_NOT_UPDATE: &str = "Couldn't reach the cloud to update this conversation. \
     Check your connection; it updates again the next time you open it.";

/// Shown when the server no longer has the conversation.
pub const NO_LONGER_EXISTS: &str = "This conversation no longer exists in the \
     cloud. It will disappear from your list shortly.";

/// Shown when an answer was refused because the input was settled already.
pub const ALREADY_ANSWERED: &str = "Already answered elsewhere.";

/// How often an open, signed-in app pulls.
pub const PULL_INTERVAL: Duration = Duration::from_secs(30);

/// A pull stops after this many pages even if the server keeps answering,
/// so a server that never reports the end cannot spin the client.
const MAX_PAGES_PER_PULL: usize = 1_000;

/// Pull every change since the engine's cursor and return what to do about
/// them, in order.
///
/// `local` is the rows as they are now; each page is matched against them
/// with the earlier pages' actions already applied. A 410 restarts the pull
/// from the beginning once ([`SyncAction::FullResync`] is among the actions);
/// a second one in the same pull is an error. On any error the engine keeps
/// the cursor of the last page it took, so the actions returned before it
/// are lost but the next pull resumes from there — the caller applies
/// nothing from a failed pull, and the rows it would have changed are
/// re-listed.
pub async fn pull<A: HiveConversationsApi + ?Sized>(
    api: &A,
    engine: &mut SyncEngine,
    local: &[ConversationMetadata],
) -> Result<Vec<SyncAction>, ApiError> {
    pull_pages(api, engine, local, PAGE_LIMIT).await
}

/// [`pull`] with `limit` changes asked for per page.
async fn pull_pages<A: HiveConversationsApi + ?Sized>(
    api: &A,
    engine: &mut SyncEngine,
    local: &[ConversationMetadata],
    limit: usize,
) -> Result<Vec<SyncAction>, ApiError> {
    let start = engine.clone();
    let mut rows = local.to_vec();
    let mut actions = Vec::new();
    let mut restarted = false;
    for _ in 0..MAX_PAGES_PER_PULL {
        let page = match api.changes_since(engine.cursor(), limit).await {
            Ok(page) => page,
            Err(ApiError::CursorExpired) if !restarted => {
                restarted = true;
                actions.extend(engine.cursor_expired());
                continue;
            }
            Err(error) => {
                // Nothing from this pull is applied, so the engine must not
                // move past changes the caller never saw.
                *engine = start;
                return Err(error);
            }
        };
        let outcome = engine.apply_page(&rows, page, limit);
        apply_to_rows(&mut rows, &outcome.actions);
        actions.extend(outcome.actions);
        if outcome.caught_up {
            return Ok(actions);
        }
    }
    tracing::warn!(
        pages = MAX_PAGES_PER_PULL,
        "a sync pull hit its page limit; the rest follows on the next pull"
    );
    Ok(actions)
}

/// Carry `actions` out on the persisted rows.
///
/// An upsert of a conversation this device has no row for writes a row with
/// no history yet, marked stale (`last_synced_at` `None`), which the detail
/// fills when the conversation is opened. Rows are read and written whole,
/// one action at a time; a frontend that also holds the conversation in
/// memory updates that copy through the store, since a later save from
/// memory would otherwise write back the old sync state.
pub async fn apply_to_repository<R: ConversationRepository + ?Sized>(
    repo: &R,
    actions: &[SyncAction],
) -> RepositoryResult<()> {
    for action in actions {
        match action {
            SyncAction::UpsertMetadata(remote) => {
                let data = match repo.load_one(&remote.local_id).await? {
                    Some(mut data) => {
                        data.title = remote.title.clone();
                        data.updated_at = remote.updated_at;
                        set_sync_columns(&mut data, &remote.sync);
                        data
                    }
                    None => {
                        let mut data = ConversationDetail {
                            title: remote.title.clone(),
                            model_id: remote.model_id.clone().unwrap_or_default(),
                            last_active_at: Some(remote.updated_at),
                            ..ConversationDetail::default()
                        }
                        .to_conversation_data(&remote.local_id);
                        // A new row has no local creation time to keep.
                        data.created_at = remote.updated_at;
                        set_sync_columns(&mut data, &remote.sync);
                        data
                    }
                };
                repo.save(&remote.local_id, data).await?;
            }
            SyncAction::ApplyTombstone { local_id } => repo.delete(local_id).await?,
            SyncAction::DetachDeleted { local_id } => {
                if let Some(mut data) = repo.load_one(local_id).await? {
                    set_sync_columns(&mut data, &ConversationSync::LocalOnly);
                    repo.save(local_id, data).await?;
                }
            }
            SyncAction::MarkDetailStale { local_id } => {
                if let Some(mut data) = repo.load_one(local_id).await? {
                    data.last_synced_at = None;
                    repo.save(local_id, data).await?;
                }
            }
            SyncAction::FullResync => {}
        }
    }
    Ok(())
}

/// Replace a loaded synced conversation's history with the server's `detail`
/// and record the version it is at (the local version is kept when the server
/// sends none), so the copy is no longer stale. Returns
/// the turn status the detail reports. Both frontends adopt a fetched detail
/// through this; the caller persists the conversation afterwards.
///
/// Fails when the conversation is not synced, or the server's history cannot
/// be adopted; the conversation is then unchanged.
pub fn adopt_detail(
    conversation: &mut crate::models::Conversation,
    detail: ConversationDetail,
    now_ms: i64,
) -> anyhow::Result<TurnStatus> {
    let mut synced = conversation
        .sync()
        .synced()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("the conversation is not synced"))?;
    let status = detail.turn_status.unwrap_or(if detail.turn_active {
        TurnStatus::Running
    } else {
        TurnStatus::Idle
    });
    let version = detail.version;
    let data = detail.to_conversation_data(conversation.id());
    conversation.replace_from_remote(data)?;
    match version {
        Some(version) => synced.version = version,
        // Resetting to 0 would make the next `If-Match` a guaranteed 412.
        None => tracing::warn!(
            local_id = %conversation.id(),
            version = synced.version,
            "The server's detail carries no version; keeping the local one"
        ),
    }
    synced.last_synced_at = Some(now_ms);
    conversation.set_sync(ConversationSync::Synced(synced));
    Ok(status)
}

fn set_sync_columns(data: &mut ConversationData, sync: &ConversationSync) {
    let columns = sync.to_columns();
    data.sync = columns.sync;
    data.placement = columns.placement;
    data.remote_version = columns.remote_version;
    data.last_synced_at = columns.last_synced_at;
}

#[cfg(test)]
mod tests;
