//! What the broker tells the root about the runs under it (TB-1, AGE-663).
//!
//! A nested worker's events reach its caller folded into that caller's
//! delegation, so the root would otherwise see a grandchild only as a line
//! in its child's progress. The broker forwards each nested run's events to
//! the root call it descends from as a [`SwarmEvent`], tagged with
//! `(root_task_id, node, chain)` from its own task table: the node is the
//! connection the events arrived on and the chain the one the broker
//! stamped on the run (DP-2), so a worker cannot name either.
//!
//! Forwarding is bounded. [`SwarmBatcher`] coalesces a node's items and the
//! broker flushes it at most once per [`FORWARD_INTERVAL`], one
//! [`SwarmEvent`] per node per flush, the way `TextBatch` coalesces stream
//! text at source (AGE-166). Text is summarised to its byte length; tool
//! starts and finishes and usage pass whole. Nothing is forwarded per
//! token.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::delegation::CallChain;

/// How often the broker flushes forwarded events: at most one
/// [`SwarmEvent`] per node per interval reaches the root.
pub const FORWARD_INTERVAL: Duration = Duration::from_millis(250);

/// One thing a nested run did, as the root receives it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SwarmItem {
    /// The worker's session started a turn.
    TurnStarted,
    /// The run's answer grew by `bytes` bytes. The text itself stays with
    /// the run's caller: it never crosses a hop to the root.
    Text {
        bytes: u64,
    },
    ToolCallStarted {
        id: String,
        name: String,
    },
    ToolCallResult {
        id: String,
        result: String,
    },
    ToolCallError {
        id: String,
        error: String,
    },
    /// The run's usage as its terminal status reported it (`metadata.usage`),
    /// whole.
    Usage {
        usage: Value,
    },
    /// The run's task ended in `state` (an A2A task state: `completed`,
    /// `failed`, `canceled`).
    Ended {
        state: String,
    },
    /// The conversation the run's terminal status carried (RC-0, AGE-649):
    /// what the full-run export nests under the call that started the run
    /// (AGE-859). Sent once, before [`Ended`](Self::Ended).
    Conversation {
        conversation: CapturedConversation,
    },
}

/// A run's captured conversation, as its terminal status carried it
/// (RC-0, AGE-649).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "captured", rename_all = "snake_case")]
pub enum CapturedConversation {
    /// Every message of every turn the run took, in order: chatty-core's
    /// messages, as JSON.
    Messages { messages: Value },
    /// The conversation was over the capture cap, so only its size came
    /// back. An export says so rather than cutting it silently.
    TooLarge { bytes: u64 },
}

impl CapturedConversation {
    /// What `metadata` carries, if the run captured anything.
    pub fn from_metadata(metadata: &crate::wire::TaskMetadata) -> Option<Self> {
        if let Some(conversation) = metadata.conversation.as_ref()
            && let Ok(messages) = conversation.to_value()
        {
            return Some(Self::Messages { messages });
        }
        metadata
            .conversation_too_large
            .map(|bytes| Self::TooLarge { bytes })
    }
}

impl SwarmItem {
    /// Whether a worker may report this item about itself: its turns and
    /// its tool events. Text, usage and the end are the broker's to read
    /// off the task's own frames, so a worker cannot claim them.
    pub fn is_workers_to_report(&self) -> bool {
        matches!(
            self,
            Self::TurnStarted
                | Self::ToolCallStarted { .. }
                | Self::ToolCallResult { .. }
                | Self::ToolCallError { .. }
        )
    }
}

/// A batch of one nested node's items, tagged by the broker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwarmEvent {
    /// The root call this run descends from (its chain's).
    pub root_task_id: String,
    /// The node that ran it: the name its connection was admitted under.
    pub node: String,
    /// The chain the broker stamped on the run (DP-2).
    pub chain: CallChain,
    /// What the node did since its last batch, in order.
    pub inner: Vec<SwarmItem>,
}

/// Coalesces tagged items per node until the next flush.
#[derive(Debug, Default)]
pub struct SwarmBatcher {
    /// One pending batch per node, in the order nodes first reported.
    pending: Vec<SwarmEvent>,
}

impl SwarmBatcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `item`, which `node` did while running under `chain`. Text
    /// right after text is one summary.
    pub fn push(&mut self, node: &str, chain: &CallChain, item: SwarmItem) {
        let index = match self.pending.iter().position(|batch| batch.node == node) {
            Some(index) => index,
            None => {
                self.pending.push(SwarmEvent {
                    root_task_id: chain.root_task_id.clone(),
                    node: node.to_string(),
                    chain: chain.clone(),
                    inner: Vec::new(),
                });
                self.pending.len() - 1
            }
        };
        let inner = &mut self.pending[index].inner;
        match (inner.last_mut(), item) {
            (Some(SwarmItem::Text { bytes }), SwarmItem::Text { bytes: more }) => *bytes += more,
            (_, item) => inner.push(item),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Every pending batch, at most one per node.
    pub fn flush(&mut self) -> Vec<SwarmEvent> {
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flush_is_one_batch_per_node_with_text_summed() {
        let root = CallChain::root("t-1");
        let a = root.extend("mid").unwrap().extend("a").unwrap();
        let b = root.extend("mid").unwrap().extend("b").unwrap();
        let mut batcher = SwarmBatcher::new();
        for _ in 0..100 {
            batcher.push("a-0", &a, SwarmItem::Text { bytes: 3 });
        }
        batcher.push(
            "b-0",
            &b,
            SwarmItem::ToolCallStarted {
                id: "c1".into(),
                name: "read_file".into(),
            },
        );
        batcher.push("a-0", &a, SwarmItem::Text { bytes: 2 });

        let batches = batcher.flush();
        assert!(batcher.is_empty());
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].node, "a-0");
        assert_eq!(batches[0].root_task_id, "t-1");
        assert_eq!(batches[0].chain, a);
        assert_eq!(batches[0].inner, [SwarmItem::Text { bytes: 302 }]);
        assert_eq!(batches[1].node, "b-0");
        assert!(batcher.flush().is_empty());
    }

    /// The rate cap itself (TB-1/AGE-663): a node fed items far faster than
    /// [`FORWARD_INTERVAL`] still reaches the root at most once per node
    /// per interval, because nothing drains [`SwarmBatcher`] but a flush
    /// gated on the same `tokio::time::interval_at` the broker's
    /// forwarding loop uses (`calls.rs`). Deterministic: the clock is
    /// paused and only ever moves because something here explicitly waits
    /// on it (`sleep`, `interval.tick()`), so this holds regardless of
    /// machine load — unlike a wall-clock measurement of a real run.
    #[tokio::test(start_paused = true)]
    async fn a_fast_node_is_flushed_at_most_once_per_interval() {
        use std::collections::BTreeMap;
        use std::sync::Arc;

        use tokio::sync::Mutex;

        const INTERVALS: u32 = 4;
        const PUSHES_PER_INTERVAL: u32 = 25;
        // Each node pushes far more often than the flush interval: a push
        // every 10ms against a 250ms flush.
        let push_every = FORWARD_INTERVAL / PUSHES_PER_INTERVAL;

        let root = CallChain::root("t-1");
        let a = root.extend("mid").unwrap().extend("a").unwrap();
        let b = root.extend("mid").unwrap().extend("b").unwrap();

        let batcher = Arc::new(Mutex::new(SwarmBatcher::new()));
        let pusher = |node: &'static str, chain: CallChain| {
            let batcher = batcher.clone();
            async move {
                for i in 0..(INTERVALS * PUSHES_PER_INTERVAL) {
                    tokio::time::sleep(push_every).await;
                    batcher
                        .lock()
                        .await
                        .push(node, &chain, SwarmItem::Text { bytes: i as u64 });
                }
            }
        };

        // The broker's own pattern (calls.rs): a flush interval, gated to
        // only fire while there is something pending.
        let flusher = {
            let batcher = batcher.clone();
            async move {
                let mut flush = tokio::time::interval_at(
                    tokio::time::Instant::now() + FORWARD_INTERVAL,
                    FORWARD_INTERVAL,
                );
                flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                let mut per_node: BTreeMap<String, usize> = BTreeMap::new();
                for _ in 0..INTERVALS {
                    flush.tick().await;
                    let mut batcher = batcher.lock().await;
                    if !batcher.is_empty() {
                        for batch in batcher.flush() {
                            *per_node.entry(batch.node).or_default() += 1;
                        }
                    }
                }
                per_node
            }
        };

        let (.., per_node) = tokio::join!(pusher("a-0", a), pusher("b-0", b), flusher);

        assert_eq!(
            per_node.keys().collect::<Vec<_>>(),
            vec!["a-0", "b-0"],
            "both fast nodes reached the root"
        );
        for (node, count) in &per_node {
            assert!(
                *count <= INTERVALS as usize,
                "{node}: {count} batches over {INTERVALS} intervals \
                 ({} pushes each): the rate cap was not enforced",
                INTERVALS * PUSHES_PER_INTERVAL
            );
        }
    }

    #[test]
    fn items_serialize_tagged_by_kind() {
        assert_eq!(
            serde_json::to_value(SwarmItem::Text { bytes: 5 }).unwrap(),
            serde_json::json!({"kind": "text", "bytes": 5})
        );
        let item = SwarmItem::ToolCallStarted {
            id: "c1".into(),
            name: "shell".into(),
        };
        assert!(item.is_workers_to_report());
        assert!(!SwarmItem::Usage { usage: Value::Null }.is_workers_to_report());
    }
}
