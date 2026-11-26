//! A scripted [`HiveConversationsApi`] for tests: an in-memory server with
//! versions, tombstones and an expiring cursor, shaped like HX-8's.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::api::{ApiError, HiveConversationsApi};
use super::wire::{
    ChangesPage, ConversationDetail, FULL_SYNC_CURSOR, RemoteChange, RemoteSummary, TurnStatus,
};
use crate::models::conversation::Placement;
use crate::repositories::conversation_repository::BoxFuture;

#[derive(Default)]
struct State {
    /// Remote id → (summary, seq of its last change).
    conversations: BTreeMap<String, (RemoteSummary, u64)>,
    /// Remote id → seq of its deletion.
    tombstones: BTreeMap<String, u64>,
    /// Monotonic change counter; a cursor is the seq of the last change a
    /// page listed.
    seq: u64,
    /// The seq of the newest tombstone pruned. A cursor at or before it is
    /// answered 410 if it was issued before that prune, so it may have
    /// skipped the pruned deletion; a full sync (`since=0`) never is, and
    /// neither is a cursor issued since, so a resync cannot loop.
    pruned_through: u64,
    /// Remote ids whose turn was resumed, in order.
    resumed: Vec<String>,
}

/// An in-memory chatty-server conversation list, with HX-8's `since` mode:
/// a cursor is `<seq of the last change listed>.<pruned_through when
/// issued>`, a page lists the
/// changes after the cursor in change order (tombstones included), at most
/// `limit` of them, and an empty page echoes the cursor it was sent.
#[derive(Clone)]
pub struct FakeHiveConversations {
    state: Arc<Mutex<State>>,
}

impl Default for FakeHiveConversations {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeHiveConversations {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    /// Create a conversation, or save a new version of it.
    pub fn save(&self, id: &str, title: &str, updated_at: i64) {
        let mut state = self.state.lock().unwrap();
        state.seq += 1;
        let seq = state.seq;
        state.tombstones.remove(id);
        let version = state
            .conversations
            .get(id)
            .map_or(1, |(summary, _)| summary.version + 1);
        state.conversations.insert(
            id.to_string(),
            (
                RemoteSummary {
                    id: id.to_string(),
                    title: title.to_string(),
                    model_id: None,
                    version,
                    updated_at,
                    turn_status: TurnStatus::Idle,
                    placement: Placement::Cloud { model_id: None },
                },
                seq,
            ),
        );
    }

    /// Mark a conversation's turn as `status`, as the server would after a
    /// restart or a new turn.
    pub fn set_turn_status(&self, id: &str, status: TurnStatus) {
        if let Some((summary, _)) = self.state.lock().unwrap().conversations.get_mut(id) {
            summary.turn_status = status;
        }
    }

    /// The conversations resumed so far, in order.
    pub fn resumed(&self) -> Vec<String> {
        self.state.lock().unwrap().resumed.clone()
    }

    /// Delete a conversation, leaving a tombstone.
    pub fn delete(&self, id: &str) {
        let mut state = self.state.lock().unwrap();
        state.seq += 1;
        let seq = state.seq;
        if state.conversations.remove(id).is_some() {
            state.tombstones.insert(id.to_string(), seq);
        }
    }

    /// Prune every tombstone so far: a cursor at or before the newest of
    /// them now expires.
    pub fn prune_tombstones(&self) {
        let mut state = self.state.lock().unwrap();
        if let Some(newest) = state.tombstones.values().copied().max() {
            state.pruned_through = state.pruned_through.max(newest);
        }
        state.tombstones.clear();
    }
}

impl HiveConversationsApi for FakeHiveConversations {
    fn changes_since(
        &self,
        cursor: &str,
        limit: usize,
    ) -> BoxFuture<'static, Result<ChangesPage, ApiError>> {
        let state = self.state.lock().unwrap();
        let parsed = if cursor == FULL_SYNC_CURSOR {
            Some((0, state.pruned_through))
        } else {
            cursor
                .split_once('.')
                .and_then(|(after, mark)| Some((after.parse().ok()?, mark.parse().ok()?)))
        };
        let result = match parsed {
            None => Err(anyhow::anyhow!("422: malformed cursor {cursor:?}").into()),
            Some((after, mark)) if mark < state.pruned_through && after <= state.pruned_through => {
                Err(ApiError::CursorExpired)
            }
            Some((after, _)) => {
                let mut changes: Vec<(u64, RemoteChange)> = state
                    .conversations
                    .values()
                    .filter(|(_, seq)| *seq > after)
                    .map(|(summary, seq)| (*seq, RemoteChange::Upsert(summary.clone())))
                    .chain(
                        state
                            .tombstones
                            .iter()
                            .filter(|(_, seq)| **seq > after)
                            .map(|(id, seq)| (*seq, RemoteChange::Deleted { id: id.clone() })),
                    )
                    .collect();
                changes.sort_by_key(|(seq, _)| *seq);
                changes.truncate(limit);
                let next_cursor = match changes.last() {
                    Some((seq, _)) => format!("{seq}.{}", state.pruned_through),
                    None => cursor.to_string(),
                };
                Ok(ChangesPage {
                    changes: changes.into_iter().map(|(_, change)| change).collect(),
                    next_cursor: Some(next_cursor),
                })
            }
        };
        Box::pin(async move { result })
    }

    fn conversation(
        &self,
        remote_id: &str,
    ) -> BoxFuture<'static, Result<ConversationDetail, ApiError>> {
        let state = self.state.lock().unwrap();
        let result = state
            .conversations
            .get(remote_id)
            .map(|(summary, _)| ConversationDetail {
                id: summary.id.clone(),
                title: summary.title.clone(),
                version: Some(summary.version),
                placement: Some(summary.placement.clone()),
                turn_status: Some(summary.turn_status),
                updated_at: Some(summary.updated_at),
                ..ConversationDetail::default()
            })
            .ok_or(ApiError::NotFound);
        Box::pin(async move { result })
    }

    fn resume(&self, remote_id: &str) -> BoxFuture<'static, Result<(), ApiError>> {
        let mut state = self.state.lock().unwrap();
        let result = match state.conversations.get_mut(remote_id) {
            Some((summary, _)) if summary.turn_status == TurnStatus::Interrupted => {
                summary.turn_status = TurnStatus::Running;
                state.resumed.push(remote_id.to_string());
                Ok(())
            }
            Some(_) => Err(ApiError::Refused(
                "This turn can't be resumed any more.".to_string(),
            )),
            None => Err(ApiError::NotFound),
        };
        Box::pin(async move { result })
    }
}
