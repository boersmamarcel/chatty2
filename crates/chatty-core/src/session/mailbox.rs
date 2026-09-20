//! What happens to a message sent while a turn is running (AGE-482).
//!
//! Every surface used to refuse it: the session bails with "a turn is
//! already running", the composers hide Send, the TUI ignores Enter. The
//! only thing that queued was the loop's own protocol follow-up, one slot,
//! first wins — and that slot existed three times. The mailbox is the one
//! queue that replaces them.
//!
//! # A pure state machine
//!
//! The mailbox does no I/O, spawns nothing and never touches the session.
//! Its owner — the TUI engine, the headless runner, the desktop controller —
//! feeds it two things, an [`Arrival`] and a [`TurnEnd`], and executes the
//! [`Decision`] it gets back with the verbs it already has: start a turn,
//! cancel a turn. That is what lets one implementation serve a gpui entity
//! and a tokio engine alike, and what lets the rules be tested as a table.
//!
//! It is generic over the message `M` because each owner queues *what it
//! sends*, not a prepared `TurnInput`: the display work a send does (the
//! user bubble, the paste expansion) belongs at dispatch time, not at
//! arrival.
//!
//! # The rules
//!
//! | Arrival | Idle | Running |
//! | -- | -- | -- |
//! | `Message` | dispatch | append; `MailboxFull` past the cap |
//! | `Interrupt` | dispatch | push front, clear the hold, cancel |
//! | `Stop` | nothing | set the hold, cancel; nothing fires on turn end |
//! | `FollowUp` | dispatch | front, one slot; a second is refused |
//! | `Withdraw` | remove | remove |
//!
//! When the turn ends: `Completed` pops the front unless held; `Cancelled`
//! pops the front only when an `Interrupt` asked for the cancel, otherwise
//! holds; `Error` holds — four queued turns must not run into the same
//! provider failure. *Held* means the queue stays visible and nothing
//! fires. Resume is not a verb: the next `Message` while idle clears the
//! hold, dispatches the queue's front and appends itself.
//!
//! Answers to clarifications and approvals are not mailbox traffic; they
//! keep their own paths.

use std::collections::VecDeque;

/// How many messages may wait. A queue that grows without limit is a UI
/// that has stopped reading its own state.
pub const DEFAULT_MAILBOX_CAP: usize = 5;

/// Identifies one queued message for the lifetime of a mailbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QueuedId(u64);

/// A message waiting its turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queued<M> {
    pub id: QueuedId,
    pub message: M,
    /// A protocol follow-up rather than something the user typed: sent
    /// without a user bubble, and it does not reset the todo protocol.
    pub follow_up: bool,
}

/// Something the owner received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arrival<M> {
    /// The user sent a message.
    Message(M),
    /// The user sent a message that should run *instead of* the current
    /// turn: cancel it, and run this one next.
    Interrupt(M),
    /// The loop wants this sent as the next turn (`SessionEvent::FollowUp`).
    FollowUp(M),
    /// The user pressed Stop. The queue is kept, but nothing fires.
    Stop,
    /// The user took a queued message back.
    Withdraw(QueuedId),
}

/// What the owner should do about an arrival.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision<M> {
    /// Start a turn with this now.
    Dispatch(Queued<M>),
    /// It waits; `position` is 1-based, for display.
    Queued {
        id: QueuedId,
        position: usize,
    },
    /// Cancel the running turn. What happens on its end is already decided
    /// inside the mailbox: [`Mailbox::turn_ended`] says what to run next.
    Cancel,
    Withdrawn(QueuedId),
    Refused(Refusal),
    Nothing,
}

/// Why an arrival was not taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The queue holds `cap` messages already.
    MailboxFull { cap: usize },
    /// No queued message has that id.
    NothingToWithdraw,
    /// A follow-up is already waiting; the loop's rule is one slot.
    FollowUpAlreadyQueued,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MailboxFull { cap } => {
                write!(f, "the queue is full ({cap} messages waiting)")
            }
            Self::NothingToWithdraw => f.write_str("no queued message to take back"),
            Self::FollowUpAlreadyQueued => f.write_str("a follow-up is already queued"),
        }
    }
}

/// How the turn ended, in the three ways the mailbox tells apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnEnd {
    Completed,
    Cancelled,
    Error,
}

/// The per-conversation queue. See the module docs for the rules.
#[derive(Debug, Clone)]
pub struct Mailbox<M> {
    queue: VecDeque<Queued<M>>,
    /// Nothing fires on turn end until the user sends again.
    held: bool,
    /// The running turn is being cancelled by an `Interrupt`, so its
    /// `Cancelled` end drains rather than holds.
    drain_on_cancel: bool,
    cap: usize,
    next_id: u64,
}

impl<M> Default for Mailbox<M> {
    fn default() -> Self {
        Self::with_cap(DEFAULT_MAILBOX_CAP)
    }
}

impl<M> Mailbox<M> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_cap(cap: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            held: false,
            drain_on_cancel: false,
            cap,
            next_id: 0,
        }
    }

    /// Decide what to do with `arrival`, given whether a turn is running.
    pub fn arrive(&mut self, arrival: Arrival<M>, turn_active: bool) -> Decision<M> {
        match arrival {
            Arrival::Message(message) => {
                if turn_active {
                    return self.enqueue_back(message, false);
                }
                // Idle with messages waiting means the queue was held (a
                // Stop, an error). Sending again is how the user resumes:
                // the front goes first, the new message waits behind it.
                if self.queue.is_empty() {
                    self.held = false;
                    return Decision::Dispatch(self.entry(message, false));
                }
                if let Decision::Refused(refusal) = self.enqueue_back(message, false) {
                    return Decision::Refused(refusal);
                }
                self.held = false;
                self.pop_front()
            }
            Arrival::Interrupt(message) => {
                self.held = false;
                if !turn_active {
                    // Nothing to interrupt; it simply goes first.
                    let entry = self.entry(message, false);
                    if self.queue.is_empty() {
                        return Decision::Dispatch(entry);
                    }
                    self.queue.push_front(entry);
                    return self.pop_front();
                }
                let entry = self.entry(message, false);
                self.queue.push_front(entry);
                self.drain_on_cancel = true;
                Decision::Cancel
            }
            Arrival::FollowUp(message) => {
                if !turn_active {
                    return Decision::Dispatch(self.entry(message, true));
                }
                if self.queue.iter().any(|q| q.follow_up) {
                    return Decision::Refused(Refusal::FollowUpAlreadyQueued);
                }
                // The loop's continuation of the turn that is ending goes
                // before anything the user queued for after it.
                let entry = self.entry(message, true);
                let id = entry.id;
                self.queue.push_front(entry);
                Decision::Queued { id, position: 1 }
            }
            Arrival::Stop => {
                if !turn_active {
                    return Decision::Nothing;
                }
                self.held = true;
                self.drain_on_cancel = false;
                Decision::Cancel
            }
            Arrival::Withdraw(id) => match self.queue.iter().position(|q| q.id == id) {
                Some(index) => {
                    self.queue.remove(index);
                    Decision::Withdrawn(id)
                }
                None => Decision::Refused(Refusal::NothingToWithdraw),
            },
        }
    }

    /// The turn ended; what, if anything, runs next.
    pub fn turn_ended(&mut self, end: TurnEnd) -> Option<Queued<M>> {
        let drain = match end {
            TurnEnd::Completed => !self.held,
            TurnEnd::Cancelled => self.drain_on_cancel,
            TurnEnd::Error => {
                self.held = true;
                false
            }
        };
        self.drain_on_cancel = false;
        if !drain {
            return None;
        }
        self.queue.pop_front()
    }

    /// Everything waiting, front first.
    pub fn pending(&self) -> impl Iterator<Item = &Queued<M>> {
        self.queue.iter()
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Whether the queue is waiting for the user rather than for the turn.
    pub fn is_held(&self) -> bool {
        self.held
    }

    /// The id of the message queued last, for a "take back the last one"
    /// affordance.
    pub fn last_queued(&self) -> Option<QueuedId> {
        self.queue.back().map(|q| q.id)
    }

    fn entry(&mut self, message: M, follow_up: bool) -> Queued<M> {
        let id = QueuedId(self.next_id);
        self.next_id += 1;
        Queued {
            id,
            message,
            follow_up,
        }
    }

    fn enqueue_back(&mut self, message: M, follow_up: bool) -> Decision<M> {
        if self.queue.len() >= self.cap {
            return Decision::Refused(Refusal::MailboxFull { cap: self.cap });
        }
        let entry = self.entry(message, follow_up);
        let id = entry.id;
        self.queue.push_back(entry);
        Decision::Queued {
            id,
            position: self.queue.len(),
        }
    }

    fn pop_front(&mut self) -> Decision<M> {
        match self.queue.pop_front() {
            Some(entry) => Decision::Dispatch(entry),
            None => Decision::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUNNING: bool = true;
    const IDLE: bool = false;

    fn mailbox() -> Mailbox<&'static str> {
        Mailbox::new()
    }

    fn dispatched(decision: Decision<&'static str>) -> &'static str {
        match decision {
            Decision::Dispatch(q) => q.message,
            other => panic!("expected a dispatch, got {other:?}"),
        }
    }

    fn queued_id(decision: Decision<&'static str>) -> QueuedId {
        match decision {
            Decision::Queued { id, .. } => id,
            other => panic!("expected a queued message, got {other:?}"),
        }
    }

    fn pending(mailbox: &Mailbox<&'static str>) -> Vec<&'static str> {
        mailbox.pending().map(|q| q.message).collect()
    }

    // --- the arrival table, idle column ---

    #[test]
    fn a_message_while_idle_is_dispatched() {
        let mut m = mailbox();
        assert_eq!(dispatched(m.arrive(Arrival::Message("hi"), IDLE)), "hi");
        assert!(m.is_empty());
    }

    #[test]
    fn an_interrupt_while_idle_is_an_ordinary_dispatch() {
        let mut m = mailbox();
        assert_eq!(dispatched(m.arrive(Arrival::Interrupt("now"), IDLE)), "now");
    }

    #[test]
    fn a_follow_up_while_idle_is_dispatched_as_a_follow_up() {
        let mut m = mailbox();
        match m.arrive(Arrival::FollowUp("continue"), IDLE) {
            Decision::Dispatch(q) => assert!(q.follow_up),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn stop_while_idle_does_nothing() {
        let mut m = mailbox();
        assert_eq!(m.arrive(Arrival::Stop, IDLE), Decision::Nothing);
        assert!(!m.is_held());
    }

    // --- the arrival table, running column ---

    #[test]
    fn a_message_while_running_is_queued_in_order() {
        let mut m = mailbox();
        assert_eq!(
            m.arrive(Arrival::Message("first"), RUNNING),
            Decision::Queued {
                id: QueuedId(0),
                position: 1
            }
        );
        assert_eq!(
            m.arrive(Arrival::Message("second"), RUNNING),
            Decision::Queued {
                id: QueuedId(1),
                position: 2
            }
        );
        assert_eq!(pending(&m), ["first", "second"]);
    }

    #[test]
    fn the_sixth_message_is_refused_and_the_queue_is_unchanged() {
        let mut m = mailbox();
        for i in 0..DEFAULT_MAILBOX_CAP {
            let _ = m.arrive(Arrival::Message(["a", "b", "c", "d", "e"][i]), RUNNING);
        }
        assert_eq!(
            m.arrive(Arrival::Message("f"), RUNNING),
            Decision::Refused(Refusal::MailboxFull {
                cap: DEFAULT_MAILBOX_CAP
            })
        );
        assert_eq!(m.len(), DEFAULT_MAILBOX_CAP);
    }

    #[test]
    fn an_interrupt_while_running_cancels_and_goes_first() {
        let mut m = mailbox();
        let _ = m.arrive(Arrival::Message("later"), RUNNING);
        assert_eq!(
            m.arrive(Arrival::Interrupt("now"), RUNNING),
            Decision::Cancel
        );
        assert_eq!(pending(&m), ["now", "later"]);
        assert_eq!(m.turn_ended(TurnEnd::Cancelled).unwrap().message, "now");
        assert_eq!(pending(&m), ["later"]);
    }

    #[test]
    fn stop_while_running_cancels_and_holds_the_queue() {
        let mut m = mailbox();
        let _ = m.arrive(Arrival::Message("later"), RUNNING);
        assert_eq!(m.arrive(Arrival::Stop, RUNNING), Decision::Cancel);
        assert!(m.is_held());
        assert!(m.turn_ended(TurnEnd::Cancelled).is_none());
        assert_eq!(pending(&m), ["later"], "the queue is kept, nothing fires");
    }

    #[test]
    fn a_follow_up_while_running_goes_to_the_front_and_has_one_slot() {
        let mut m = mailbox();
        let _ = m.arrive(Arrival::Message("user"), RUNNING);
        assert_eq!(
            m.arrive(Arrival::FollowUp("loop"), RUNNING),
            Decision::Queued {
                id: QueuedId(1),
                position: 1
            }
        );
        assert_eq!(
            m.arrive(Arrival::FollowUp("loop again"), RUNNING),
            Decision::Refused(Refusal::FollowUpAlreadyQueued)
        );
        let next = m.turn_ended(TurnEnd::Completed).unwrap();
        assert_eq!((next.message, next.follow_up), ("loop", true));
        assert_eq!(pending(&m), ["user"]);
    }

    #[test]
    fn withdraw_removes_the_named_message_and_refuses_unknown_ids() {
        let mut m = mailbox();
        let first = queued_id(m.arrive(Arrival::Message("first"), RUNNING));
        let _ = m.arrive(Arrival::Message("second"), RUNNING);
        assert_eq!(
            m.arrive(Arrival::Withdraw(first), RUNNING),
            Decision::Withdrawn(first)
        );
        assert_eq!(pending(&m), ["second"]);
        assert_eq!(
            m.arrive(Arrival::Withdraw(first), RUNNING),
            Decision::Refused(Refusal::NothingToWithdraw)
        );
        assert_eq!(m.last_queued(), Some(QueuedId(1)));
    }

    // --- turn_ended ---

    #[test]
    fn a_completed_turn_drains_one_message_at_a_time() {
        let mut m = mailbox();
        let _ = m.arrive(Arrival::Message("first"), RUNNING);
        let _ = m.arrive(Arrival::Message("second"), RUNNING);
        assert_eq!(m.turn_ended(TurnEnd::Completed).unwrap().message, "first");
        assert_eq!(m.turn_ended(TurnEnd::Completed).unwrap().message, "second");
        assert!(m.turn_ended(TurnEnd::Completed).is_none());
    }

    #[test]
    fn an_error_holds_the_queue() {
        let mut m = mailbox();
        let _ = m.arrive(Arrival::Message("later"), RUNNING);
        assert!(m.turn_ended(TurnEnd::Error).is_none());
        assert!(m.is_held());
        assert_eq!(pending(&m), ["later"]);
    }

    #[test]
    fn a_cancel_nobody_asked_for_holds_the_queue() {
        // A hosted cancel, or the owner's own stop path: without an
        // Interrupt the cancelled end is a Stop.
        let mut m = mailbox();
        let _ = m.arrive(Arrival::Message("later"), RUNNING);
        assert!(m.turn_ended(TurnEnd::Cancelled).is_none());
        assert_eq!(pending(&m), ["later"]);
    }

    #[test]
    fn sending_while_held_dispatches_the_front_and_appends_the_new_message() {
        let mut m = mailbox();
        let _ = m.arrive(Arrival::Message("older"), RUNNING);
        let _ = m.arrive(Arrival::Stop, RUNNING);
        assert!(m.turn_ended(TurnEnd::Cancelled).is_none());

        assert_eq!(
            dispatched(m.arrive(Arrival::Message("newer"), IDLE)),
            "older"
        );
        assert!(!m.is_held());
        assert_eq!(pending(&m), ["newer"]);
    }

    #[test]
    fn an_interrupt_after_a_stop_wins() {
        let mut m = mailbox();
        let _ = m.arrive(Arrival::Stop, RUNNING);
        assert_eq!(
            m.arrive(Arrival::Interrupt("now"), RUNNING),
            Decision::Cancel
        );
        assert_eq!(m.turn_ended(TurnEnd::Cancelled).unwrap().message, "now");
    }

    #[test]
    fn a_stop_after_an_interrupt_wins() {
        let mut m = mailbox();
        let _ = m.arrive(Arrival::Interrupt("now"), RUNNING);
        let _ = m.arrive(Arrival::Stop, RUNNING);
        assert!(m.turn_ended(TurnEnd::Cancelled).is_none());
        assert_eq!(pending(&m), ["now"]);
    }

    #[test]
    fn the_drain_flag_does_not_outlive_the_turn_it_was_set_for() {
        let mut m = mailbox();
        let _ = m.arrive(Arrival::Interrupt("now"), RUNNING);
        let _ = m.turn_ended(TurnEnd::Cancelled);
        // The interrupting turn is running now; a plain cancel of it holds.
        let _ = m.arrive(Arrival::Message("later"), RUNNING);
        assert!(m.turn_ended(TurnEnd::Cancelled).is_none());
    }
}
