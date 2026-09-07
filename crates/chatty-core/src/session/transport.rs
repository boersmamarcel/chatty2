//! Dispatching one turn to wherever the conversation runs (AGE-298).
//!
//! [`ConversationMode`](crate::models::conversation::ConversationMode) is what
//! is *persisted*; these are the two lines of code that mode actually selects.
//!
//! # What is and is not different about a hosted conversation
//!
//! Only who runs the stream. A frontend keeps its [`AgentSession`] either way:
//! the session owns the local [`Conversation`](crate::models::conversation::Conversation),
//! applies every event to it, and finishes the turn — and it must, or a
//! conversation would stop having a local row the moment it went online and
//! there would be nothing to bring back. What a hosted conversation *adds* is
//! a [`HostedSession`] beside it, and the only questions that then have two
//! answers are "who opens the stream", "who hears a cancel", and "where does
//! an approval go". Those are the four functions here.
//!
//! # Why free functions and not a trait
//!
//! A trait object cannot carry this: the turn future has to stay `Send` for
//! chatty-tui (`tokio::spawn`) while `emit` must *not* be `Send` for
//! chatty-gpui, whose event sink holds an `AsyncApp`. A boxed `dyn Future`
//! has to pick one and either choice breaks a frontend.
//! [`Either`](futures::future::Either) keeps both, because it is `Send`
//! exactly when both arms are — so each frontend gets the auto-traits its own
//! executor needs, inferred rather than declared.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::Result;
use futures::future::Either;

use super::event::SessionEvent;
use super::hosted::HostedSession;
use super::{AgentSession, TurnInput};
use crate::models::clarification_store::ClarificationAnswer;
use crate::models::execution_approval_store::ApprovalDecision;
use crate::models::write_approval_store::WriteApprovalDecision;

/// Start a turn, in-process or on the server.
///
/// `hosted` is `Some` exactly when the conversation's mode is
/// [`Hosted`](crate::models::conversation::ConversationMode::Hosted). The
/// future is `Send` when `F` is, so each frontend can spawn it on its own
/// executor.
pub fn begin_turn<F: FnMut(SessionEvent)>(
    session: &mut AgentSession,
    hosted: Option<&mut HostedSession>,
    input: TurnInput,
    cancel_flag: Arc<AtomicBool>,
    emit: F,
) -> Result<impl Future<Output = ()> + use<F>> {
    Ok(match hosted {
        Some(remote) => {
            // The server runs the stream, but the local session still has to
            // know a turn is running: it commits the user message now and
            // finalizes the reply later, so the conversation's own row
            // records the turn. The remote goes first because its refusals
            // (a turn already running there, attachments with no text) leave
            // nothing behind, whereas an opened local turn would.
            let turn = remote.begin_turn_with_flag(&input, cancel_flag.clone(), emit)?;
            session.open_hosted_turn(&input, cancel_flag)?;
            Either::Right(turn)
        }
        None => Either::Left(session.begin_turn_with_flag(input, cancel_flag, emit)?),
    })
}

/// Is a turn running, wherever it runs?
///
/// This is the guard on the move: AGE-298 refuses one mid-turn, because a
/// pending approval or clarification lives in the stores of the session that
/// raised it and moving would leave it with no address an answer could name.
pub fn is_turn_active(session: &AgentSession, hosted: Option<&HostedSession>) -> bool {
    match hosted {
        Some(remote) => remote.is_turn_active(),
        None => session.is_turn_active(),
    }
}

/// Stop the running turn.
///
/// The local flag is set before this returns either way, so a caller that
/// drops the future still stops the turn it can reach; the future is the POST
/// that tells a server about it.
pub fn cancel(
    session: &AgentSession,
    hosted: Option<&HostedSession>,
) -> impl Future<Output = ()> + use<> {
    match hosted {
        Some(remote) => Either::Right(remote.cancel()),
        None => {
            session.cancel();
            Either::Left(std::future::ready(()))
        }
    }
}

/// Answer an approval the turn in flight raised.
pub fn resolve_approval(
    session: &AgentSession,
    hosted: Option<&HostedSession>,
    id: &str,
    approved: bool,
) -> impl Future<Output = ()> + use<> {
    match hosted {
        Some(remote) => Either::Right(remote.resolve_approval(id, approved)),
        None => {
            // Execution and write approvals share an id space, so try one and
            // fall back — the same rule the server applies at its own edge.
            let execution = if approved {
                ApprovalDecision::Approved
            } else {
                ApprovalDecision::Denied
            };
            if !session.execution_approvals().resolve(id, execution) {
                let write = if approved {
                    WriteApprovalDecision::Approved
                } else {
                    WriteApprovalDecision::Denied
                };
                session.write_approvals().resolve(id, write);
            }
            Either::Left(std::future::ready(()))
        }
    }
}

/// Answer an `ask_user` the turn in flight raised.
pub fn resolve_clarification(
    session: &AgentSession,
    hosted: Option<&HostedSession>,
    id: &str,
    answers: Vec<ClarificationAnswer>,
) -> impl Future<Output = ()> + use<> {
    match hosted {
        Some(remote) => Either::Right(remote.resolve_clarification(id, answers)),
        None => {
            session.clarifications().resolve(id, answers);
            Either::Left(std::future::ready(()))
        }
    }
}
