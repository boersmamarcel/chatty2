//! [`SyncEngine`]: the pure half of a sync pull (AGE-2088).
//!
//! Local rows and one page of the server's changes go in; [`SyncAction`]s
//! come out. No I/O, no clock: [`pull`](super::pull) feeds it pages and the
//! frontend applies what it returns.

use std::collections::HashSet;

use super::wire::{ChangesPage, FULL_SYNC_CURSOR, RemoteChange, RemoteSummary, TurnStatus};
use crate::models::conversation::{ConversationSync, SyncedConversation};
use crate::repositories::ConversationMetadata;

/// What one page asks of the local rows.
#[derive(Clone, Debug, PartialEq)]
pub enum SyncAction {
    /// Create the row, or update its title, time and sync state. The history
    /// is not touched: a version newer than the one held comes with a
    /// [`MarkDetailStale`](Self::MarkDetailStale) as well.
    UpsertMetadata(RemoteRow),
    /// The conversation was deleted on the server: delete this device's copy.
    ApplyTombstone { local_id: String },
    /// The server has a newer version of the history than this device holds:
    /// fetch the detail before showing it as current.
    MarkDetailStale { local_id: String },
    /// The cursor expired (410): the pull starts again from the beginning,
    /// and every synced row the full listing does not mention is deleted when
    /// it completes.
    FullResync,
}

/// A conversation's metadata as the server describes it, addressed to the
/// local row it belongs on.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteRow {
    /// The local row: an existing one synced to this remote id, or — for a
    /// conversation this device has not seen — the remote id itself.
    pub local_id: String,
    pub title: String,
    pub model_id: Option<String>,
    /// Unix seconds.
    pub updated_at: i64,
    pub sync: ConversationSync,
    /// Live, not persisted.
    pub turn_status: TurnStatus,
    /// The device holding a device-placed conversation, by name. Live, not
    /// persisted.
    pub device_name: Option<String>,
}

/// What [`SyncEngine::apply_page`] decided about one page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageOutcome {
    pub actions: Vec<SyncAction>,
    /// The page reached the end of the server's changes: the pull is done.
    pub caught_up: bool,
}

/// The sync state machine for one server.
///
/// It owns the cursor, which lives in memory only: a fresh engine starts at
/// [`FULL_SYNC_CURSOR`], so every app start is a full sync, which lists every
/// conversation and every tombstone the server still holds. Only a resync
/// after a 410 also deletes the synced rows it did not see, since that is
/// the one case where the server has pruned a tombstone this device missed.
///
/// Only rows synced to *this* server are ever matched, updated or deleted. A
/// local-only row is never touched, and neither is one synced elsewhere.
#[derive(Clone, Debug)]
pub struct SyncEngine {
    server_url: String,
    cursor: String,
    /// `Some` during a resync after a 410: the remote ids it has listed so
    /// far.
    resync: Option<HashSet<String>>,
}

impl SyncEngine {
    pub fn new(server_url: &str) -> Self {
        Self {
            server_url: server_url.trim_end_matches('/').to_string(),
            cursor: FULL_SYNC_CURSOR.to_string(),
            resync: None,
        }
    }

    pub fn server_url(&self) -> &str {
        &self.server_url
    }

    /// Where the next page starts; [`FULL_SYNC_CURSOR`] lists from the
    /// beginning.
    pub fn cursor(&self) -> &str {
        &self.cursor
    }

    /// The server answered 410: start over from the beginning.
    pub fn cursor_expired(&mut self) -> Vec<SyncAction> {
        self.cursor = FULL_SYNC_CURSOR.to_string();
        self.resync = Some(HashSet::new());
        vec![SyncAction::FullResync]
    }

    /// The rows synced to this server, by remote id.
    fn synced_here<'a>(
        &self,
        local: &'a [ConversationMetadata],
    ) -> impl Iterator<Item = (&'a ConversationMetadata, &'a SyncedConversation)> {
        let server_url = self.server_url.clone();
        local.iter().filter_map(move |row| match &row.sync {
            ConversationSync::Synced(synced) if synced.server_url == server_url => {
                Some((row, synced))
            }
            _ => None,
        })
    }

    /// Decide what one page of changes does to `local`. `limit` is the page
    /// size the page was asked for.
    ///
    /// `local` must reflect every action returned for earlier pages of the
    /// same pull, so a conversation created on page one is matched, not
    /// created again, when page two edits it.
    pub fn apply_page(
        &mut self,
        local: &[ConversationMetadata],
        page: ChangesPage,
        limit: usize,
    ) -> PageOutcome {
        let mut actions = Vec::new();
        // The end of the changes: a page shorter than asked for (an empty
        // one included), or a cursor that did not move.
        let caught_up = page.changes.len() < limit
            || page
                .next_cursor
                .as_deref()
                .is_none_or(|next| next == self.cursor);

        for change in page.changes {
            let matched = self
                .synced_here(local)
                .find(|(_, synced)| synced.remote_id == change.id())
                .map(|(row, synced)| (row.id.clone(), synced.clone()));
            match change {
                RemoteChange::Deleted { .. } => {
                    if let Some((local_id, _)) = matched {
                        actions.push(SyncAction::ApplyTombstone { local_id });
                    }
                }
                RemoteChange::Upsert(summary) => {
                    if let Some(seen) = &mut self.resync {
                        seen.insert(summary.id.clone());
                    }
                    actions.extend(self.upsert(local, matched, summary));
                }
            }
        }

        if let Some(cursor) = page.next_cursor {
            self.cursor = cursor;
        }
        if caught_up && let Some(seen) = self.resync.take() {
            // The server pruned tombstones this device never saw, so a
            // deletion can only show as an absence: a resync lists every
            // conversation the server has, and a synced row it never
            // mentioned was deleted there.
            actions.extend(
                self.synced_here(local)
                    .filter(|(_, synced)| !seen.contains(&synced.remote_id))
                    .filter(|(row, _)| {
                        !actions.iter().any(|action| {
                            matches!(action, SyncAction::ApplyTombstone { local_id } if *local_id == row.id)
                        })
                    })
                    .map(|(row, _)| SyncAction::ApplyTombstone {
                        local_id: row.id.clone(),
                    })
                    .collect::<Vec<_>>(),
            );
        }
        PageOutcome { actions, caught_up }
    }

    fn upsert(
        &self,
        local: &[ConversationMetadata],
        matched: Option<(String, SyncedConversation)>,
        summary: RemoteSummary,
    ) -> Vec<SyncAction> {
        let (local_id, held) = match matched {
            Some((local_id, held)) => (local_id, Some(held)),
            // A conversation this device has not seen takes its remote id as
            // its local id, unless a row of another origin already has that
            // id.
            None => {
                let local_id = if local.iter().any(|row| row.id == summary.id) {
                    format!("hive-{}", summary.id)
                } else {
                    summary.id.clone()
                };
                (local_id, None)
            }
        };
        if let Some(held) = &held
            && summary.version < held.version
        {
            // Older than what this device already holds: a reordered page.
            return Vec::new();
        }
        let stale = held
            .as_ref()
            .is_some_and(|held| summary.version > held.version);
        let sync = ConversationSync::Synced(SyncedConversation {
            server_url: self.server_url.clone(),
            remote_id: summary.id,
            // The history this device holds is still the version it was;
            // only fetching the detail moves it.
            version: held.as_ref().map_or(0, |held| held.version),
            placement: summary.placement,
            last_synced_at: if stale {
                None
            } else {
                held.and_then(|held| held.last_synced_at)
            },
        });
        let mut actions = vec![SyncAction::UpsertMetadata(RemoteRow {
            local_id: local_id.clone(),
            title: summary.title,
            model_id: summary.model_id,
            updated_at: summary.updated_at,
            sync,
            turn_status: summary.turn_status,
            device_name: summary.device_name,
        })];
        if stale {
            actions.push(SyncAction::MarkDetailStale { local_id });
        }
        actions
    }
}

/// Apply `actions` to metadata rows, keeping them newest first: what the
/// store's sidebar layer does with a pull, and what each next page of the
/// same pull must be matched against.
pub fn apply_to_rows(rows: &mut Vec<ConversationMetadata>, actions: &[SyncAction]) {
    for action in actions {
        match action {
            SyncAction::UpsertMetadata(remote) => {
                match rows.iter_mut().find(|row| row.id == remote.local_id) {
                    Some(row) => {
                        row.title = remote.title.clone();
                        row.updated_at = remote.updated_at;
                        row.sync = remote.sync.clone();
                    }
                    None => rows.push(ConversationMetadata {
                        id: remote.local_id.clone(),
                        title: remote.title.clone(),
                        total_cost: 0.0,
                        updated_at: remote.updated_at,
                        sync: remote.sync.clone(),
                        tool_call_count: 0,
                        context_tokens: 0,
                    }),
                }
            }
            SyncAction::ApplyTombstone { local_id } => rows.retain(|row| row.id != *local_id),
            SyncAction::MarkDetailStale { local_id } => {
                if let Some(ConversationSync::Synced(synced)) = rows
                    .iter_mut()
                    .find(|row| row.id == *local_id)
                    .map(|row| &mut row.sync)
                {
                    synced.last_synced_at = None;
                }
            }
            SyncAction::FullResync => {}
        }
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.updated_at));
}
