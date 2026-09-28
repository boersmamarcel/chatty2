//! Messages waiting for their recipient (tree messages, TM-1).
//!
//! `send_message` never starts a run and is never delivered mid-run: a
//! message waits on its recipient's [`PendingList`] until a delivery point
//! takes it. The list is bounded twice, and an over-limit message is refused
//! whole, never truncated:
//!
//! - at most [`PENDING_LIST_BYTES`] wait for one recipient at a time, and
//! - one sender may add at most [`SENDER_ALLOWANCE_BYTES`] per run of the
//!   recipient, whether or not earlier messages were delivered since.
//!
//! A message's size is its text's length in bytes (UTF-8).

use std::collections::{HashMap, VecDeque};

use crate::directory::{NodeId, NodeName};
use crate::transport::RefusalReason;

/// What may wait for one recipient at once: 64 KB.
pub const PENDING_LIST_BYTES: usize = 64 * 1024;

/// What one sender may add to one recipient's list per run of the
/// recipient: 8 KB.
pub const SENDER_ALLOWANCE_BYTES: usize = 8 * 1024;

/// One message waiting for delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The broker-assigned id `send_message` returned for it.
    pub id: String,
    pub from: NodeId,
    /// The sender's broker-assigned name, which delivery shows the recipient.
    pub from_name: NodeName,
    pub text: String,
}

impl Message {
    fn bytes(&self) -> usize {
        self.text.len()
    }
}

/// The messages waiting for one recipient, in the order they were accepted.
#[derive(Debug, Clone, Default)]
pub struct PendingList {
    /// The bytes of `items`.
    bytes: usize,
    /// The bytes each sender added during the recipient's current run.
    per_sender: HashMap<NodeId, usize>,
    items: VecDeque<Message>,
}

impl PendingList {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue `message`, or refuse it with [`RefusalReason::OverAllowance`]
    /// if it would take the list past [`PENDING_LIST_BYTES`] or its sender
    /// past [`SENDER_ALLOWANCE_BYTES`] for this run. A refused message
    /// leaves the list as it was.
    pub fn push(&mut self, message: Message) -> Result<(), RefusalReason> {
        let size = message.bytes();
        let sent = self.per_sender.get(&message.from).copied().unwrap_or(0);
        if sent + size > SENDER_ALLOWANCE_BYTES || self.bytes + size > PENDING_LIST_BYTES {
            return Err(RefusalReason::OverAllowance);
        }
        self.per_sender.insert(message.from, sent + size);
        self.bytes += size;
        self.items.push_back(message);
        Ok(())
    }

    /// Take every waiting message, oldest first: what a delivery point
    /// hands the recipient. Frees the list's bytes; the senders' allowances
    /// for this run stay spent.
    pub fn take_all(&mut self) -> Vec<Message> {
        self.bytes = 0;
        self.items.drain(..).collect()
    }

    /// The recipient started a new run: every sender's allowance is whole
    /// again. Waiting messages stay.
    pub fn start_run(&mut self) {
        self.per_sender.clear();
    }

    /// The bytes waiting.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory::{ConversationScope, Directory};

    const KB: usize = 1024;

    fn message(from: &crate::directory::Node, n: usize, size: usize) -> Message {
        Message {
            id: format!("msg-{n}"),
            from: from.id(),
            from_name: from.name().clone(),
            text: "x".repeat(size),
        }
    }

    /// Invariant 4: the 9th KB from one sender in one run is refused, and so
    /// is the 65th KB in total; neither is truncated.
    #[test]
    fn pending_list_bounds() {
        let mut dir = Directory::new();
        let scope = ConversationScope::new("c1");
        let owner = dir.admit("leader", None, scope.clone()).unwrap();
        let senders: Vec<_> = (0..9)
            .map(|_| {
                dir.admit("local-coder", Some(owner.id()), scope.clone())
                    .unwrap()
            })
            .collect();
        let mut list = PendingList::new();
        let mut n = 0;
        let mut next = |from, size| {
            n += 1;
            message(from, n, size)
        };

        // One sender: eight 1 KB messages fit, the ninth KB does not.
        for _ in 0..8 {
            list.push(next(&senders[0], KB)).unwrap();
        }
        assert_eq!(
            list.push(next(&senders[0], KB)),
            Err(RefusalReason::OverAllowance),
            "the 9th KB from one sender in one run"
        );
        assert_eq!(
            list.push(next(&senders[0], 1)),
            Err(RefusalReason::OverAllowance),
            "not even one byte more: nothing is truncated to fit"
        );
        assert_eq!(list.bytes(), 8 * KB);
        assert_eq!(list.len(), 8);

        // Delivery frees the list but not the sender's allowance for this
        // run; the recipient's next run does.
        let delivered = list.take_all();
        assert_eq!(delivered.len(), 8);
        assert_eq!(delivered[0].id, "msg-1", "oldest first");
        assert!(list.is_empty());
        assert_eq!(list.bytes(), 0);
        assert_eq!(
            list.push(next(&senders[0], KB)),
            Err(RefusalReason::OverAllowance),
            "the allowance is per run, not per delivery"
        );
        list.start_run();
        list.push(next(&senders[0], 8 * KB))
            .expect("a new run gives the sender its whole allowance back");
        list.start_run();

        // In total: eight senders fill 64 KB, and the 65th KB — from a
        // ninth sender well inside its own allowance — is refused.
        for sender in &senders[1..8] {
            list.push(next(sender, 8 * KB)).unwrap();
        }
        assert_eq!(list.bytes(), 64 * KB);
        let before = list.len();
        assert_eq!(
            list.push(next(&senders[8], KB)),
            Err(RefusalReason::OverAllowance),
            "the 65th KB in total"
        );
        assert_eq!(list.len(), before, "a refused message is not queued");

        // An empty message always fits.
        list.push(next(&senders[8], 0)).unwrap();
    }
}
