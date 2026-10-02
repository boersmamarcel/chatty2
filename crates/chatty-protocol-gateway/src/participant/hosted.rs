//! The hosted broker: the root-broker core, embedded by a host that serves
//! one hosted root conversation with it (HS-4a, AGE-685; ADR-0020,
//! ADR-0021 § 3, ADR-0023 § 3).
//!
//! The local broker is built by the desktop around an in-process root: a
//! [`DirectTransport`](super::DirectTransport), a [`LocalRunner`] per role,
//! and a socket pair per worker. A hosted broker is the same core —
//! [`ParticipantRegistry`] (the `chatty-fabric` directory, task table and
//! permits), [`BrokerCalls`] and [`serve_connection`] — built by hive's
//! `chatty-server`, one per hosted root conversation, over connections it
//! accepted itself (a vsock stream per lease). It binds no listener of its
//! own: no Unix socket, no gateway, no TCP port.
//!
//! What differs is the root and the policy:
//!
//! * **The root is the hosted client.** There is no in-process root. The
//!   host-side root process answers only authenticated requests from the
//!   user's client, and each one reaches the broker through [`HostedRoot`],
//!   as a [`Caller::HostedRoot`] carrying the session's verified `(tenant,
//!   user)`, decided by [`decide`](super::decide) like every other request.
//!   A client whose `(tenant, user)` is not the broker's [`Binding`] is
//!   refused at the handle. An answer to a question or approval names its
//!   pending id and the nonce the broker handed out with it, never the
//!   session alone (ADR-0021 § 2).
//! * **A real policy, always.** [`HostedConfig::policy`] is a
//!   [`CallPolicy`] by value, not an `Option`, and
//!   [`LocalPermissive`](super::LocalPermissive) cannot be built outside
//!   this crate: without a real policy a hosted broker does not start
//!   (ADR-0021, Migration step 4).
//! * **Every decision is written before its effect.** [`DecisionLog`] is
//!   the host's audit sink; a write that fails refuses the request as
//!   `internal` (ADR-0023 § 7).
//! * **Every task carries the binding.** Each task the broker hands a node
//!   carries the binding's [`TaskIdentity`], and no other (ADR-0021 § 3).
//!
//! # Connect, then build
//!
//! The host admits a node (from HS-1, its spec) when it creates the lease —
//! [`HostedBroker::admit`] — and serves the one connection the lease's
//! worker makes with [`HostedBroker::serve`]. The worker says
//! `session.hello` with this build's schema hash, gets `welcome` with the
//! name it was admitted under, and only then builds its agent around the
//! connection's transport ([`crate::worker::WorkerConnection`]).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use chatty_fabric::wire::TaskIdentity;
use chatty_fabric::{
    AgentOrigin, Answer, ApprovalVerdict, CallError, CallEvent, CallPolicy, CallRequest,
    DirectoryError, EdgeLog,
};
use futures::StreamExt;
use futures::stream::BoxStream;

use super::calls::{BrokerCalls, Peer};
use super::gate::{Binding, Caller, ConversationOp, Decision, HostedClient, Request};
use super::registry::{AdmittedNode, ParticipantRegistry};
use super::virtual_agent::VirtualAgent;

#[cfg(doc)]
use super::{LocalRunner, serve_connection};

/// One decision, as a hosted broker writes it to its [`DecisionLog`]:
/// the typed caller, the row it matched, and the outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRecord {
    /// The typed caller, e.g. `root:acme/ada` or `node:analyst-0`.
    pub caller: String,
    /// The row, e.g. `root/agent.invoke`.
    pub row: String,
    /// `None` when granted; the refusal otherwise.
    pub refused: Option<String>,
}

/// Where a hosted broker writes every decision, before its effect.
///
/// An `Err` refuses the request it records as `internal` (ADR-0023 § 7): a
/// hosted broker does nothing it could not log.
pub trait DecisionLog: Send + Sync {
    fn record(&self, record: &DecisionRecord) -> Result<(), String>;
}

/// What a hosted broker is built from.
pub struct HostedConfig {
    /// The `(tenant, user)` of the hosted root conversation it serves, and
    /// the kind of root its runs have.
    pub binding: Binding,
    /// The delegation policy every call is checked against. Required.
    pub policy: Arc<dyn CallPolicy>,
    /// Where every decision is written before its effect.
    pub log: Arc<dyn DecisionLog>,
    /// The edge log, if the host keeps one.
    pub edges: Option<Arc<Mutex<EdgeLog>>>,
}

/// A hosted broker's own state, held by its [`BrokerCalls`].
pub(crate) struct Hosted {
    pub(crate) binding: Binding,
    log: Arc<dyn DecisionLog>,
    /// The nonce handed out with each question or approval pending at the
    /// hosted root, by the broker's id for it.
    nonces: Mutex<HashMap<String, String>>,
}

impl Hosted {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.nonces.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The nonce handed out with pending id `id`, if one was.
    pub(crate) fn nonce_of(&self, id: &str) -> Option<String> {
        self.lock().get(id).cloned()
    }

    /// Write `decision`, made for `caller`, to the host's log.
    pub(crate) fn record(&self, caller: &Caller, decision: &Decision) -> Result<(), String> {
        self.log.record(&DecisionRecord {
            caller: caller.to_string(),
            row: decision.row.to_string(),
            refused: decision
                .outcome
                .as_ref()
                .err()
                .map(|refused| refused.to_call_error().to_string()),
        })
    }

    /// Every task this broker hands a node carries this identity.
    pub(crate) fn identity(&self) -> TaskIdentity {
        TaskIdentity::new(&self.binding.tenant, &self.binding.user)
    }

    /// A fresh nonce for pending id `id`.
    fn mint(&self, id: &str) -> String {
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        self.lock().insert(id.to_string(), nonce.clone());
        nonce
    }

    fn forget(&self, id: &str) {
        self.lock().remove(id);
    }
}

/// The root-broker core for one hosted root conversation (HS-4a).
///
/// Holds no listener: the host accepts each worker's connection itself and
/// hands it to [`serve`](Self::serve).
pub struct HostedBroker {
    registry: ParticipantRegistry,
    calls: Arc<BrokerCalls>,
}

impl HostedBroker {
    /// Build a hosted broker over `registry`, reaching `runners` — the
    /// host's virtual agents (a microVM runner per role), which make their
    /// workers in the same `registry`.
    pub fn new(
        config: HostedConfig,
        registry: ParticipantRegistry,
        runners: BTreeMap<String, Arc<dyn VirtualAgent>>,
    ) -> Self {
        let HostedConfig {
            binding,
            policy,
            log,
            edges,
        } = config;
        let hosted = Hosted {
            binding,
            log,
            nonces: Mutex::default(),
        };
        let calls = Arc::new(BrokerCalls::new_hosted(
            registry.clone(),
            Arc::new(runners),
            edges,
            policy,
            hosted,
        ));
        registry.install_calls(&calls);
        Self { registry, calls }
    }

    /// The registry the broker's nodes are admitted and registered in.
    pub fn registry(&self) -> &ParticipantRegistry {
        &self.registry
    }

    /// Who this broker serves.
    pub fn binding(&self) -> &Binding {
        &self.hosted().binding
    }

    /// The hosted root's API: the only way the hosted client reaches the
    /// broker.
    pub fn root(&self) -> HostedRoot {
        HostedRoot {
            calls: self.calls.clone(),
        }
    }

    /// Admit a node started as `spec` for `owner` (a node's name, or
    /// `None` for the root), before its lease exists: the name the worker
    /// will be welcomed under. Serve its connection with
    /// [`serve`](Self::serve); dropping the node unserved abandons it via
    /// [`ParticipantRegistry::abandon`].
    pub fn admit(&self, spec: &str, owner: Option<&str>) -> Result<AdmittedNode, DirectoryError> {
        self.registry.admit(spec, AgentOrigin::Fleet, owner)
    }

    /// Serve `node`'s one connection, `stream`, from its `session.hello`
    /// to its last frame (see [`serve_connection`]). Returns when the
    /// connection closes, which deregisters the node and fails its open
    /// tasks.
    #[cfg(unix)]
    pub fn serve<S>(
        &self,
        stream: S,
        node: AdmittedNode,
    ) -> impl std::future::Future<Output = ()> + Send + 'static
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
    {
        super::serve_connection(stream, self.registry.clone(), node)
    }

    fn hosted(&self) -> &Hosted {
        self.calls
            .hosted_state()
            .expect("a hosted broker's calls are hosted")
    }
}

/// One event on a hosted root call, with the nonce an answer to it must
/// name: set on a [`CallEvent::Ask`] or [`CallEvent::Approve`], `None`
/// otherwise. The host relays both to the user's client, and only to it.
#[derive(Debug)]
pub struct HostedEvent {
    pub event: CallEvent,
    pub nonce: Option<String>,
}

/// What [`HostedRoot::call`] returns: progress, then one result or error.
/// Dropping it cancels whatever the call started.
pub type HostedStream = BoxStream<'static, Result<HostedEvent, CallError>>;

/// The hosted root's API (ADR-0023 § 3, "Hosted client"): each request is
/// made as the session-authenticated `client` the host passes, decided
/// against the broker's [`Binding`] before any effect.
#[derive(Clone)]
pub struct HostedRoot {
    calls: Arc<BrokerCalls>,
}

impl HostedRoot {
    fn hosted(&self) -> &Hosted {
        self.calls
            .hosted_state()
            .expect("a hosted broker's calls are hosted")
    }

    /// Start a run (`agent.invoke`) or list the agents (`agent.list`) as
    /// `client`. A refused request is a stream of its one error.
    pub fn call(&self, client: &HostedClient, request: CallRequest) -> HostedStream {
        let caller = Caller::HostedRoot(client.clone());
        let calls = self.calls.clone();
        self.calls
            .call_as(Peer::Root, caller, request)
            .map(move |event| {
                let hosted = calls.hosted_state().expect("hosted");
                event.map(|event| {
                    let nonce = match &event {
                        CallEvent::Ask { id, .. } | CallEvent::Approve { id, .. } => {
                            Some(hosted.mint(id))
                        }
                        CallEvent::InputWithdrawn { id } => {
                            hosted.forget(id);
                            None
                        }
                        CallEvent::Progress(_) | CallEvent::Swarm(_) | CallEvent::Result(_) => None,
                    };
                    HostedEvent { event, nonce }
                })
            })
            .boxed()
    }

    /// Answer question `id` with `answers`, naming its `nonce`.
    pub fn answer(
        &self,
        client: &HostedClient,
        id: &str,
        nonce: Option<&str>,
        answers: Vec<Answer>,
    ) -> Result<(), CallError> {
        self.decide(client, &Request::Answer { id, nonce })?;
        self.hosted().forget(id);
        self.calls.answer_question(id, answers)
    }

    /// Answer approval `id` with `verdict`, naming its `nonce`.
    pub fn approve(
        &self,
        client: &HostedClient,
        id: &str,
        nonce: Option<&str>,
        verdict: ApprovalVerdict,
    ) -> Result<(), CallError> {
        self.decide(client, &Request::AnswerApproval { id, nonce })?;
        self.hosted().forget(id);
        self.calls.answer_approval(id, verdict)
    }

    /// Stop `node` and everything under it (TB-7).
    pub fn cancel(&self, client: &HostedClient, node: &str) -> Result<(), CallError> {
        self.decide(client, &Request::Cancel { node })?;
        self.calls.cancel(node)
    }

    /// Read the root's waiting run messages, for its next turn (TM-2).
    pub fn read(&self, client: &HostedClient) -> Result<Vec<String>, CallError> {
        self.decide(client, &Request::TakeRunMessages)?;
        Ok(self.calls.start_run(&Peer::Root))
    }

    /// Decide whether `client` may create, list or delete this hosted root
    /// conversation; the host performs it only on `Ok`.
    pub fn conversation(&self, client: &HostedClient, op: ConversationOp) -> Result<(), CallError> {
        self.decide(client, &Request::Conversation(op))
    }

    fn decide(&self, client: &HostedClient, request: &Request<'_>) -> Result<(), CallError> {
        self.calls
            .root_request_as(&Caller::HostedRoot(client.clone()), request)
    }
}

#[cfg(all(test, unix))]
#[path = "hosted_tests.rs"]
mod tests;
