//! A scripted [`HiveConversationsApi`] for tests: an in-memory server with
//! versions, tombstones and an expiring cursor, shaped like HX-8's.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::api::{ApiError, HiveConversationsApi};
use super::wire::{ChangesPage, ConversationDetail, RemoteChange, RemoteSummary, TurnStatus};
use crate::models::conversation::Placement;
use crate::repositories::conversation_repository::BoxFuture;

#[derive(Default)]
struct State {
    /// Remote id → (summary, seq of its last change).
    conversations: BTreeMap<String, (RemoteSummary, u64)>,
    /// Remote id → seq of its deletion.
    tombstones: BTreeMap<String, u64>,
    /// Monotonic change counter; a cursor is the seq it was issued at.
    seq: u64,
    /// Cursors at or below this answer 410, as if their tombstones were
    /// pruned.
    expired_through: u64,
    page_size: usize,
}

/// An in-memory chatty-server conversation list.
///
/// Cursors are opaque strings (see `parse_cursor`); a page lists changes after the
/// cursor in change order, at most `page_size` of them.
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
            state: Arc::new(Mutex::new(State {
                page_size: 100,
                ..State::default()
            })),
        }
    }

    /// At most `size` changes per page, to exercise paging.
    pub fn with_page_size(self, size: usize) -> Self {
        self.state.lock().unwrap().page_size = size.max(1);
        self
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

    /// Delete a conversation, leaving a tombstone.
    pub fn delete(&self, id: &str) {
        let mut state = self.state.lock().unwrap();
        state.seq += 1;
        let seq = state.seq;
        if state.conversations.remove(id).is_some() {
            state.tombstones.insert(id.to_string(), seq);
        }
    }

    /// Prune every tombstone so far: every cursor issued until now expires.
    pub fn prune_tombstones(&self) {
        let mut state = self.state.lock().unwrap();
        state.tombstones.clear();
        state.expired_through = state.seq;
    }
}

/// A cursor is `seq-<last change listed>@<seq when issued>`. Expiry goes by
/// when it was issued, not by the change it points at: a full listing pages
/// through old conversations, and its cursors must not expire for that.
fn parse_cursor(cursor: &str) -> (u64, u64) {
    let (after, issued) = cursor
        .strip_prefix("seq-")
        .and_then(|rest| rest.split_once('@'))
        .unwrap_or(("0", "0"));
    (after.parse().unwrap_or(0), issued.parse().unwrap_or(0))
}

impl HiveConversationsApi for FakeHiveConversations {
    fn changes_since(
        &self,
        cursor: Option<&str>,
    ) -> BoxFuture<'static, Result<ChangesPage, ApiError>> {
        let state = self.state.lock().unwrap();
        let parsed = cursor.map(parse_cursor);
        let result = if parsed.is_some_and(|(_, issued)| issued < state.expired_through) {
            Err(ApiError::CursorExpired)
        } else {
            let after = parsed.map_or(0, |(after, _)| after);
            let mut changes: Vec<(u64, RemoteChange)> = state
                .conversations
                .values()
                .filter(|(_, seq)| *seq > after)
                .map(|(summary, seq)| (*seq, RemoteChange::Upsert(summary.clone())))
                .collect();
            // A listing from the start never carries tombstones: there is
            // nothing for it to delete.
            if cursor.is_some() {
                changes.extend(
                    state
                        .tombstones
                        .iter()
                        .filter(|(_, seq)| **seq > after)
                        .map(|(id, seq)| (*seq, RemoteChange::Deleted { id: id.clone() })),
                );
            }
            changes.sort_by_key(|(seq, _)| *seq);
            changes.truncate(state.page_size);
            let last = changes.last().map_or(after, |(seq, _)| *seq);
            Ok(ChangesPage {
                changes: changes.into_iter().map(|(_, change)| change).collect(),
                next_cursor: Some(format!("seq-{last}@{}", state.seq)),
            })
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
}
