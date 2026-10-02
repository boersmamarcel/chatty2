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
//!   [`Outbound::Call`] for the connection's writer, and returns a stream fed
//!   by whatever the broker answers under that id.
//! - [`CallReplies`] is what the connection's reader hands the broker's
//!   `req.progress`, results and errors to. Several calls can be in flight
//!   at once; each reply lands on the call its id names, whatever order they
//!   finish in.
//! - [`SocketTransport::ask`] queues a question this worker relays from a
//!   third-party A2A peer as an [`Outbound::Ask`], which the connection
//!   writes as a `human.ask` request (EN-2b); its answers come back through
//!   [`CallReplies::answered`]. It numbers its questions from the same
//!   counter as the worker's own `ask_user` questions
//!   ([`SocketTransport::question_numbers`]), so the two never collide on
//!   the connection.
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

use chatty_fabric::{Answer, AskRequest, CallError, CallEvent, CallRequest, CallStream, Transport};
use futures::StreamExt;
use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::tools::invoke_agent_tool::InvokeAgentProgress;

/// What the worker sends the broker about its calls.
#[derive(Debug, Clone, PartialEq)]
pub enum Outbound {
    /// A call: the body of a `call` frame.
    Call { id: u64, request: CallRequest },
    /// A question this worker relays from a third-party peer: a `human.ask`
    /// request, numbered `id` (EN-2b).
    Ask { id: u64, request: AskRequest },
    /// Question `id` is withdrawn: nobody waits on its answers any more.
    CancelAsk { id: u64 },
}

type Pending = Arc<Mutex<HashMap<u64, mpsc::UnboundedSender<Result<CallEvent, CallError>>>>>;

/// Question number → where its answers go.
type Asks = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Vec<Answer>, CallError>>>>>;

/// Calls over a broker-made connection. See the module docs.
pub struct SocketTransport {
    outbound: mpsc::UnboundedSender<Outbound>,
    pending: Pending,
    asks: Asks,
    next_id: AtomicU64,
    /// Every question this worker asks is numbered from this counter.
    questions: Arc<AtomicU64>,
}

/// Where the connection's reader delivers the broker's replies. Cheap to
/// clone; every clone feeds the same calls.
#[derive(Clone)]
pub struct CallReplies {
    pending: Pending,
    asks: Asks,
}

impl SocketTransport {
    /// A transport, the queue its calls and answers go out on, and the
    /// handle replies come back in through. The caller owns the connection:
    /// it writes each [`Outbound`] as a request or a `req.cancel` and hands
    /// each reply to [`CallReplies`].
    pub fn new() -> (Arc<Self>, mpsc::UnboundedReceiver<Outbound>, CallReplies) {
        let (outbound, calls) = mpsc::unbounded_channel();
        let pending: Pending = Arc::default();
        let asks: Asks = Arc::default();
        let transport = Arc::new(Self {
            outbound,
            pending: pending.clone(),
            asks: asks.clone(),
            next_id: AtomicU64::new(1),
            questions: Arc::default(),
        });
        (transport, calls, CallReplies { pending, asks })
    }

    /// The counter every question this worker asks is numbered from: the
    /// questions relayed here and its own `ask_user` questions, which the
    /// connection's task mapper numbers (EN-2b). The number before the
    /// first is 0.
    pub fn question_numbers(&self) -> Arc<AtomicU64> {
        self.questions.clone()
    }
}

/// Withdraws a question whose asker stopped waiting before its answers
/// came, so the broker takes it off whoever's screen it is on.
struct Withdraw {
    asks: Asks,
    outbound: mpsc::UnboundedSender<Outbound>,
    id: u64,
}

impl Drop for Withdraw {
    fn drop(&mut self) {
        if self.asks.lock().remove(&self.id).is_some() {
            let _ = self.outbound.send(Outbound::CancelAsk { id: self.id });
        }
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
        if self
            .outbound
            .send(Outbound::Call { id, request: req })
            .is_err()
        {
            return Err(CallError::Disconnected(
                "the connection to the broker is closed".to_string(),
            ));
        }
        Ok(async_stream::stream! {
            let _forget = forget;
            while let Some(item) = rx.recv().await {
                let last = !matches!(item, Ok(CallEvent::Progress(_)));
                yield item;
                if last {
                    break;
                }
            }
        }
        .boxed())
    }

    /// Ask the human a third-party peer's question as this worker's
    /// `human.ask` (EN-2b), and wait for the answers. Dropping the wait
    /// withdraws it.
    async fn ask(&self, request: AskRequest) -> Option<Result<Vec<Answer>, CallError>> {
        let id = self.questions.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = oneshot::channel();
        self.asks.lock().insert(id, tx);
        let withdraw = Withdraw {
            asks: self.asks.clone(),
            outbound: self.outbound.clone(),
            id,
        };
        if self.outbound.send(Outbound::Ask { id, request }).is_err() {
            self.asks.lock().remove(&id);
            return Some(Err(CallError::Disconnected(
                "the connection to the broker is closed".to_string(),
            )));
        }
        let answers = rx.await.unwrap_or_else(|_| {
            Err(CallError::Disconnected(
                "the connection to the broker closed before the question was answered".to_string(),
            ))
        });
        drop(withdraw);
        Some(answers)
    }
}

impl CallReplies {
    /// `req.progress` for call `id`.
    pub fn progress(&self, id: u64, event: Value) {
        self.deliver(id, Ok(CallEvent::Progress(event)), false);
    }

    /// The result of question `id`, if it is one [`SocketTransport::ask`]
    /// asked: `false` for any other question number.
    pub fn answered(&self, id: u64, answers: Result<Vec<Answer>, CallError>) -> bool {
        let Some(tx) = self.asks.lock().remove(&id) else {
            return false;
        };
        let _ = tx.send(answers);
        true
    }

    /// A result: call `id` is over.
    pub fn result(&self, id: u64, result: Value) {
        self.deliver(id, Ok(CallEvent::Result(result)), true);
    }

    /// An error: call `id` failed and is over.
    pub fn error(&self, id: u64, error: CallError) {
        self.deliver(id, Err(error), true);
    }

    /// The connection closed: every call and question still in flight
    /// fails.
    pub fn disconnected(&self) {
        // Dropping a question's sender fails its wait.
        self.asks.lock().clear();
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
            spawn_context: None,
            remaining: Default::default(),
        })
    }

    #[tokio::test]
    async fn replies_land_on_the_call_their_id_names() {
        let (transport, mut calls, replies) = SocketTransport::new();
        let a = transport.call(invoke("a")).await.unwrap();
        let b = transport.call(invoke("b")).await.unwrap();
        let Some(Outbound::Call { id: first, request }) = calls.recv().await else {
            panic!("a call goes out first");
        };
        let Some(Outbound::Call { id: second, .. }) = calls.recv().await else {
            panic!("then the second");
        };
        assert_ne!(first, second);
        assert_eq!(request, invoke("a"));

        // b finishes first; a gets a progress event, then its result.
        replies.result(second, json!("b done"));
        replies.progress(first, json!({"Text": "working"}));
        replies.result(first, json!("a done"));

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
        assert!(matches!(
            events.as_slice(),
            [Err(CallError::Disconnected(_))]
        ));
    }

    #[tokio::test]
    async fn a_dropped_call_is_forgotten() {
        let (transport, mut calls, replies) = SocketTransport::new();
        drop(transport.call(CallRequest::ListAgents).await.unwrap());
        let Some(Outbound::Call { id, .. }) = calls.recv().await else {
            panic!("the call went out");
        };
        assert!(replies.pending.lock().is_empty());
        // Harmless: nobody is waiting.
        replies.result(id, json!([]));
    }

    /// EN-2b: a peer's question goes up as a numbered `human.ask`, its
    /// answers come back to the wait that asked, and a wait dropped first
    /// withdraws its question.
    #[tokio::test]
    async fn a_relayed_question_is_answered_or_withdrawn() {
        let (transport, mut calls, replies) = SocketTransport::new();
        // A number the worker's own `ask_user` took first.
        transport.question_numbers().fetch_add(1, Ordering::Relaxed);
        let request = AskRequest {
            questions: Vec::new(),
            asker: None,
            origin: None,
        };
        let asking = tokio::spawn({
            let (transport, request) = (transport.clone(), request.clone());
            async move { transport.ask(request).await }
        });
        assert_eq!(
            calls.recv().await,
            Some(Outbound::Ask {
                id: 2,
                request: request.clone(),
            })
        );
        assert!(!replies.answered(1, Ok(Vec::new())), "not the transport's");
        let answers = vec![Answer {
            id: "q1".into(),
            answer: "SQLite".into(),
            custom: false,
        }];
        assert!(replies.answered(2, Ok(answers.clone())));
        assert_eq!(asking.await.unwrap(), Some(Ok(answers)));

        let gave_up = tokio::spawn({
            let transport = transport.clone();
            async move { transport.ask(request).await }
        });
        assert!(matches!(
            calls.recv().await,
            Some(Outbound::Ask { id: 3, .. })
        ));
        gave_up.abort();
        assert_eq!(calls.recv().await, Some(Outbound::CancelAsk { id: 3 }));
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
