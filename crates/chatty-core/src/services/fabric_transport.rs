//! A worker's [`Transport`]: calls over the connection its broker made
//! (ADR-0020, BI-4, AGE-636).
//!
//! A worker's `invoke_agent` and `list_agents` reach local roles through the
//! broker that spawned it, over the socket pair it was handed — never over
//! loopback HTTP. The connection already names the worker, so a call says
//! nothing about who is calling: the broker executes it as that node.
//!
//! This crate does not speak the participant protocol's frames (the gateway
//! does, and depends on this crate, not the other way round), so the
//! transport here is only the call bookkeeping:
//!
//! - [`SocketTransport::call`] gives each call an id, queues an
//!   [`OutboundCall`] for the connection's writer, and returns a stream fed
//!   by whatever the broker answers under that id.
//! - [`CallReplies`] is what the connection's reader hands the broker's
//!   `call_progress` / `call_result` / `call_error` frames to. Several calls
//!   can be in flight at once; each reply lands on the call its id names,
//!   whatever order they finish in.
//!
//! The gateway's worker loop (`chatty_protocol_gateway::worker`) owns both
//! ends of the socket and does the framing.
//!
//! [`InvokeAgentProgress`] travels as JSON inside a call's progress events
//! (`chatty-fabric` cannot name this crate's types); [`progress_to_value`]
//! and [`progress_from_value`] are the conversion at this edge.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chatty_fabric::{CallError, CallEvent, CallRequest, CallStream, Transport};
use futures::StreamExt;
use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::tools::invoke_agent_tool::InvokeAgentProgress;

/// A call the worker is making: the body of a `call` frame.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboundCall {
    pub id: u64,
    pub request: CallRequest,
}

type Pending = Arc<Mutex<HashMap<u64, mpsc::UnboundedSender<Result<CallEvent, CallError>>>>>;

/// Calls over a broker-made connection. See the module docs.
pub struct SocketTransport {
    outbound: mpsc::UnboundedSender<OutboundCall>,
    pending: Pending,
    next_id: AtomicU64,
}

/// Where the connection's reader delivers the broker's replies. Cheap to
/// clone; every clone feeds the same calls.
#[derive(Clone)]
pub struct CallReplies {
    pending: Pending,
}

impl SocketTransport {
    /// A transport, the queue its calls go out on, and the handle replies
    /// come back in through. The caller owns the connection: it writes each
    /// [`OutboundCall`] as a `call` frame and hands each reply frame to
    /// [`CallReplies`].
    pub fn new() -> (
        Arc<Self>,
        mpsc::UnboundedReceiver<OutboundCall>,
        CallReplies,
    ) {
        let (outbound, calls) = mpsc::unbounded_channel();
        let pending: Pending = Arc::default();
        let transport = Arc::new(Self {
            outbound,
            pending: pending.clone(),
            next_id: AtomicU64::new(1),
        });
        (transport, calls, CallReplies { pending })
    }
}

/// Forgets a call when its stream is dropped, so a reply that arrives after
/// the caller gave up is dropped rather than queued forever.
struct Forget {
    pending: Pending,
    id: u64,
}

impl Drop for Forget {
    fn drop(&mut self) {
        self.pending.lock().remove(&self.id);
    }
}

#[async_trait::async_trait]
impl Transport for SocketTransport {
    async fn call(&self, req: CallRequest) -> Result<CallStream, CallError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, mut rx) = mpsc::unbounded_channel();
        self.pending.lock().insert(id, tx);
        let forget = Forget {
            pending: self.pending.clone(),
            id,
        };
        if self.outbound.send(OutboundCall { id, request: req }).is_err() {
            return Err(CallError::Disconnected(
                "the connection to the broker is closed".to_string(),
            ));
        }
        Ok(async_stream::stream! {
            let _forget = forget;
            while let Some(item) = rx.recv().await {
                let last = !matches!(
                    item,
                    Ok(CallEvent::Progress(_)) | Ok(CallEvent::InputRequired { .. })
                );
                yield item;
                if last {
                    break;
                }
            }
        }
        .boxed())
    }
}

impl CallReplies {
    /// A `call_progress` frame for call `id`.
    pub fn progress(&self, id: u64, event: Value) {
        self.deliver(id, Ok(CallEvent::Progress(event)), false);
    }

    /// A `call_result` frame: call `id` is over.
    pub fn result(&self, id: u64, result: Value) {
        self.deliver(id, Ok(CallEvent::Result(result)), true);
    }

    /// A `call_error` frame: call `id` failed and is over.
    pub fn error(&self, id: u64, error: CallError) {
        self.deliver(id, Err(error), true);
    }

    /// The connection closed: every call still in flight fails.
    pub fn disconnected(&self) {
        let pending: Vec<_> = self.pending.lock().drain().collect();
        for (_, tx) in pending {
            let _ = tx.send(Err(CallError::Disconnected(
                "the connection to the broker closed mid-call".to_string(),
            )));
        }
    }

    fn deliver(&self, id: u64, item: Result<CallEvent, CallError>, last: bool) {
        let mut pending = self.pending.lock();
        let tx = if last {
            pending.remove(&id)
        } else {
            pending.get(&id).cloned()
        };
        drop(pending);
        match tx {
            Some(tx) => {
                let _ = tx.send(item);
            }
            None => tracing::debug!(call = id, "A reply for a call nobody is waiting on"),
        }
    }
}

/// An [`InvokeAgentProgress`] as a call's progress event.
pub fn progress_to_value(progress: &InvokeAgentProgress) -> Value {
    serde_json::to_value(progress).unwrap_or(Value::Null)
}

/// A call's progress event as an [`InvokeAgentProgress`], if it is one.
pub fn progress_from_value(value: Value) -> Option<InvokeAgentProgress> {
    serde_json::from_value(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatty_fabric::InvokeAgentParams;
    use serde_json::json;

    fn invoke(agent: &str) -> CallRequest {
        CallRequest::InvokeAgent(InvokeAgentParams {
            agent: agent.to_string(),
            prompt: "go".to_string(),
            handle: None,
            include_trace: false,
        })
    }

    #[tokio::test]
    async fn replies_land_on_the_call_their_id_names() {
        let (transport, mut calls, replies) = SocketTransport::new();
        let a = transport.call(invoke("a")).await.unwrap();
        let b = transport.call(invoke("b")).await.unwrap();
        let first = calls.recv().await.unwrap();
        let second = calls.recv().await.unwrap();
        assert_ne!(first.id, second.id);
        assert_eq!(first.request, invoke("a"));

        // b finishes first; a gets a progress event, then its result.
        replies.result(second.id, json!("b done"));
        replies.progress(first.id, json!({"Text": "working"}));
        replies.result(first.id, json!("a done"));

        let b: Vec<_> = b.collect().await;
        assert_eq!(b, vec![Ok(CallEvent::Result(json!("b done")))]);
        let a: Vec<_> = a.collect().await;
        assert_eq!(
            a,
            vec![
                Ok(CallEvent::Progress(json!({"Text": "working"}))),
                Ok(CallEvent::Result(json!("a done"))),
            ]
        );
    }

    #[tokio::test]
    async fn a_closed_connection_fails_every_call_in_flight() {
        let (transport, _calls, replies) = SocketTransport::new();
        let call = transport.call(CallRequest::ListAgents).await.unwrap();
        replies.disconnected();
        let events: Vec<_> = call.collect().await;
        assert!(matches!(events.as_slice(), [Err(CallError::Disconnected(_))]));
    }

    #[tokio::test]
    async fn a_dropped_call_is_forgotten() {
        let (transport, mut calls, replies) = SocketTransport::new();
        drop(transport.call(CallRequest::ListAgents).await.unwrap());
        let call = calls.recv().await.unwrap();
        assert!(replies.pending.lock().is_empty());
        // Harmless: nobody is waiting.
        replies.result(call.id, json!([]));
    }

    #[test]
    fn progress_round_trips_through_json() {
        let step = InvokeAgentProgress::Step("\u{2713} read_file".to_string());
        assert_eq!(
            progress_to_value(&step),
            json!({"Step": "\u{2713} read_file"})
        );
        assert!(matches!(
            progress_from_value(json!({"Text": "hi"})),
            Some(InvokeAgentProgress::Text(t)) if t == "hi"
        ));
        assert!(progress_from_value(json!({"Nope": 1})).is_none());
    }
}
