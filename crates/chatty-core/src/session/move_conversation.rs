//! Moving one conversation between local and hosted (AGE-298).
//!
//! Both directions are a transfer of history followed by a single flip of
//! [`ConversationMode`]. That order is the whole safety property: nothing
//! local is touched until the far side has accepted the history and named it,
//! so a client killed mid-upload leaves the local conversation exactly as it
//! was and no half-imported hosted one behind it. Re-running a move that
//! already succeeded is a no-op rather than a second copy.
//!
//! # What does not move
//!
//! Workspace files, attachments, MCP servers, memory, skills and provider
//! keys. Not "not yet handled" — *refused*, and stated to the user before
//! they confirm (see [`MoveSummary`]). A local workspace path means nothing
//! on a server; provider keys never leave this process, because hosted turns
//! use the server's own egress (AGE-284).

use anyhow::{Context, Result, bail};
use rig_core::completion::Message;
use serde::Deserialize;

use crate::models::conversation::ConversationMode;

/// What a move will and will not carry, for the confirmation the user sees.
///
/// This is data, not prose, so the desktop dialog and the TUI prompt state
/// the same thing and cannot drift from each other or from the code.
pub struct MoveSummary {
    pub moves: &'static [&'static str],
    pub does_not_move: &'static [(&'static str, &'static str)],
}

/// Taking a conversation online is a data egress: the user confirms it with
/// this in front of them.
pub const TAKE_ONLINE_SUMMARY: MoveSummary = MoveSummary {
    moves: &[
        "Message history, traces, timestamps and token usage",
        "The model, by id — the server picks its own if it does not have that one",
    ],
    does_not_move: &[
        (
            "Workspace files",
            "a local path means nothing on the server",
        ),
        (
            "Attachments and artifacts",
            "the server has no artifact sink yet",
        ),
        (
            "MCP servers, memory and skills",
            "these belong to a per-user store the server does not have yet",
        ),
        (
            "Provider API keys",
            "never — hosted turns bill through the server's own egress",
        ),
    ],
};

/// Bringing one back carries the same history in the other direction, and
/// leaves behind the same things it never had.
pub const BRING_BACK_SUMMARY: MoveSummary = MoveSummary {
    moves: &["Message history and traces recorded while it ran hosted"],
    does_not_move: &[(
        "Anything the hosted run wrote outside the conversation",
        "the server's workspace is not this machine's",
    )],
};

/// The server's `ConversationDetail`, as much of it as a move needs.
#[derive(Debug, Deserialize)]
pub struct RemoteConversation {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub model_id: String,
    #[serde(default)]
    pub turn_active: bool,
    #[serde(default)]
    pub messages: Vec<Message>,
}

/// Upload a local conversation's history and return the mode that now
/// describes it.
///
/// The caller flips the conversation to the returned mode *and only then*
/// persists it. Until this returns `Ok`, nothing local has changed.
///
/// `turn_active` is the caller's: the move is refused mid-turn because a
/// pending approval or clarification lives in the stores of the session that
/// raised it, and moving would orphan it with no address any answer could
/// name.
pub async fn take_online(
    server_url: &str,
    title: &str,
    messages: &[Message],
) -> Result<ConversationMode> {
    let http = reqwest::Client::new();
    let server_url = server_url.trim_end_matches('/');
    let url = format!("{server_url}/api/conversations");

    // One route, carrying the history it should start from: AGE-281's route
    // table stays as it is rather than growing an import verb beside it.
    let response = http
        .post(&url)
        .json(&serde_json::json!({ "title": title, "messages": messages }))
        .send()
        .await
        .with_context(|| format!("could not reach the server at {url}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        bail!(
            "the server refused the import ({status}): {}",
            detail.trim()
        );
    }

    let created: RemoteConversation = response
        .json()
        .await
        .context("the server's reply to the import was not a conversation")?;

    Ok(ConversationMode::Hosted {
        server_url: server_url.to_string(),
        remote_id: created.id,
    })
}

/// Fetch a hosted conversation's history so the caller can rebuild the local
/// one from it.
///
/// The reverse direction is cheaper because the server already exposes the
/// history: there is nothing to upload, only to read back.
pub async fn fetch_hosted(server_url: &str, remote_id: &str) -> Result<RemoteConversation> {
    let http = reqwest::Client::new();
    let server_url = server_url.trim_end_matches('/');
    let url = format!("{server_url}/api/conversations/{remote_id}");

    let response = http
        .get(&url)
        .send()
        .await
        .with_context(|| format!("could not reach the server at {url}"))?;

    if !response.status().is_success() {
        let status = response.status();
        bail!("the server would not return the conversation ({status})");
    }

    response
        .json()
        .await
        .context("the server's conversation was not in the expected shape")
}

/// What every frontend says when a move is asked for while hosted
/// conversations are off (AGE-308).
///
/// The move is developer-only until online mode is account-scoped: today it
/// carries the transcript and nothing else, and the server does not import
/// history, so a moved conversation cannot answer "what did we discuss".
pub const HOSTED_DISABLED: &str = "Hosted conversations are not enabled.";

/// Why a move cannot happen right now, in words the UI can show as-is.
pub fn refuse_reason(
    turn_active: bool,
    mode: &ConversationMode,
    going_online: bool,
) -> Option<&'static str> {
    if turn_active {
        // A turn can be blocked on an approval or a clarification that lives
        // in the stores of the session that raised it.
        return Some("This conversation is mid-turn. Wait for it to finish, or stop it first.");
    }
    match (mode.is_hosted(), going_online) {
        (true, true) => Some("This conversation is already running online."),
        (false, false) => Some("This conversation is already running locally."),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_move_is_refused_mid_turn_in_either_direction() {
        let local = ConversationMode::Local;
        assert!(
            refuse_reason(true, &local, true)
                .unwrap()
                .contains("mid-turn")
        );
        let hosted = ConversationMode::Hosted {
            server_url: "http://localhost:8081".into(),
            remote_id: "r-1".into(),
        };
        assert!(
            refuse_reason(true, &hosted, false)
                .unwrap()
                .contains("mid-turn")
        );
    }

    #[test]
    fn a_move_to_where_it_already_is_is_refused_not_repeated() {
        // Idempotence at the UI edge: taking an online conversation online
        // again must not mint a second hosted copy of it.
        let hosted = ConversationMode::Hosted {
            server_url: "http://localhost:8081".into(),
            remote_id: "r-1".into(),
        };
        assert!(refuse_reason(false, &hosted, true).is_some());
        assert!(refuse_reason(false, &ConversationMode::Local, false).is_some());
    }

    #[test]
    fn a_legitimate_move_is_allowed_in_both_directions() {
        let hosted = ConversationMode::Hosted {
            server_url: "http://localhost:8081".into(),
            remote_id: "r-1".into(),
        };
        assert!(refuse_reason(false, &ConversationMode::Local, true).is_none());
        assert!(refuse_reason(false, &hosted, false).is_none());
    }

    #[test]
    fn the_confirmation_says_provider_keys_never_move() {
        // AGE-298: "Upload provider keys, user_secrets, or workspace
        // contents. Ever." — the dialog has to say so, so the text is pinned.
        let keys = TAKE_ONLINE_SUMMARY
            .does_not_move
            .iter()
            .find(|(what, _)| what.contains("Provider API keys"))
            .expect("the egress confirmation must mention provider keys");
        assert!(keys.1.contains("never"));
        assert!(
            TAKE_ONLINE_SUMMARY
                .does_not_move
                .iter()
                .any(|(what, _)| what.contains("Workspace files"))
        );
    }
}
