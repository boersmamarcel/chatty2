//! The broker gate: one pure decision over a typed caller before any
//! effect (ADR-0023 §§ 1–4, 7; GT-0, AGE-798; GT-0b, AGE-810).
//!
//! Every request that reaches the broker over a worker's connection or the
//! root's [`DirectTransport`](super::DirectTransport) is decided here, by
//! [`decide`], before anything is spawned, submitted, queued or answered.
//! The broker resolves who is calling from what authenticated the request —
//! the connection the broker made for a node, and the task table; the
//! direct handle for the root — never from the request, reads a
//! [`Snapshot`] of what the decision needs, and acts only on a [`Grant`],
//! on the target the grant names. A [`Refused`] request has no effect.
//!
//! `decide` is synchronous and pure: it reads the caller, the typed request
//! and the snapshot, and holds no registry, runner or I/O handle. So it
//! takes no quota either: a quota-consuming effect (a mailbox slot) takes
//! its quota atomically with the effect, after the grant.
//!
//! # Callers
//!
//! | Caller | Who |
//! |---|---|
//! | [`Caller::Root`] | The local in-process root, through its direct handle |
//! | [`Caller::HostedRoot`] | The hosted root: a session-authenticated web client with its verified `(tenant, user)`, through a hosted broker's [`HostedRoot`](super::HostedRoot) API (HS-4a) |
//! | [`Caller::Node`] | A process on a broker-made connection with a run the broker opened |
//! | [`Caller::Remote`] | An ADR-0022 stream peer (GT-2; refused until then) |
//! | [`Caller::External`] | Carries its admitter: a hive key (GT-3), the local gateway's launch token (GT-1), or [`Admitter::Chainless`] |
//!
//! A broker-made connection with no open run is `External{Chainless}`, and
//! is refused everywhere. `Remote` and `External` callers are never looked
//! up or glob-matched as specs, `*` included: no policy is consulted for
//! them.
//!
//! # Rows
//!
//! | Caller | `agent.invoke` | `agent.list` | `mailbox.post` | `mailbox.take` | `human.approve`, `human.ask` | Local root requests |
//! |---|---|---|---|---|---|---|
//! | `Root` | Root policy, chain, budget, then (spawning) roster and spawn context | Granted | Any live node, at its next tool round (TM-5) | Refused (its messages open its next run) | Answers its pending ones | Granted |
//! | `Node` | The named run must be its own open run; `may_call` from its admitted spec, chain, budget, then (spawning) roster and spawn context | Granted | Its owner (TM-2's delivery points), or its own live child at the child's next tool round (TM-5) | Granted: its own mid-run list | Raises one from the task it serves (its open run) | Refused |
//! | `Remote` | Refused | Refused | Refused | Refused | Refused | Refused |
//! | `External` | Refused | Refused | Refused | Refused | Refused | Refused |
//! | hosted `Root` | As `Root` | Granted | Refused (`not_on_tree`) | Refused | Answers its pending ones, each with its nonce | Cancel, read, and conversation create, list and delete |
//!
//! On a typed-root broker the hosted client may only cancel and read
//! (take its run messages); every other row refuses it.
//! The hosted client is refused every row unless its `(tenant, user)`
//! matches the broker's [`Binding`] (and, on an `External`-rooted broker,
//! the key's owner). A local `Root` on a broker with a binding is refused:
//! a hosted broker's only root is its hosted client (ADR-0023 § 3).
//!
//! A mid-run message (TM-5) is the human's (the local root's, to any node
//! of its broker: a local broker serves one user) or an owner's, to its own
//! child; every other sender keeps TM-2's delivery points, and a sibling is
//! still `not_on_tree`. Whoever sent it, it grants nothing.
//!
//! The local root requests are its own: answering a question or an
//! approval the broker delivered to it, stopping one run, and taking its
//! run's waiting messages.
//!
//! `human.approve` (EN-2a, ADR-0021 § 2) is raised by a node from the task
//! it serves, whose run's chain stamps the asker; a refused one is denied
//! without asking. `human.approve` names no run on the wire, so a node
//! serving several tasks at once is refused one: which run asks would be a
//! guess. `human.ask` (EN-2b) follows the same rule: a node asks from the
//! task it serves, whose run's chain stamps the asker, and the root only
//! answers.
//! `session.hello` is admission (ADR-0020): the connection the broker made
//! is what admits a node, before any request.
//!
//! # Fail-closed
//!
//! A snapshot that does not add up ([`Unreadable`]) refuses as a distinct
//! [`Refused::Internal`], which the broker logs as an error. An absent
//! local policy is [`LocalPermissive`](super::LocalPermissive), not an
//! error.
//!
//! This module denies `clippy::wildcard_enum_match_arm`: a new caller,
//! request or callee kind fails to compile until every row says what it
//! does with it.

#![deny(clippy::wildcard_enum_match_arm)]

use std::fmt;
use std::time::SystemTime;

use chatty_fabric::{
    ApprovalRequest, AskRequest, CallChain, CallError, CallPolicy, InvokeAgentParams, NodeId,
    ROOT_NAME, Refusal, RefusalReason, RunId, SendMessageParams, SpawnContext,
};

use super::protocol::CallStamp;
use super::spawn_context::{self, Target};

/// Who a request is made as, resolved per request from what authenticated
/// it (ADR-0023 § 2).
#[derive(Debug, Clone, PartialEq)]
pub enum Caller {
    /// The local in-process root, through its direct handle.
    Root,
    /// The hosted root's client, as the hosted root's API authenticated
    /// it (ADR-0023 § 2: `Root`, hosted). Its kind is [`CallerKind::Root`].
    HostedRoot(HostedClient),
    /// A process on a broker-made connection with a broker-made chain.
    Node(NodeCaller),
    /// An ADR-0022 stream peer. Its claim result arrives with GT-2; until
    /// then every row refuses it.
    Remote(RemoteCaller),
    /// A caller from outside the tree, with whoever admitted it.
    External(Admitter),
}

impl Caller {
    /// The caller's kind, as a row names it.
    pub fn kind(&self) -> CallerKind {
        match self {
            Self::Root | Self::HostedRoot(_) => CallerKind::Root,
            Self::Node(_) => CallerKind::Node,
            Self::Remote(_) => CallerKind::Remote,
            Self::External(_) => CallerKind::External,
        }
    }
}

impl fmt::Display for Caller {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Root => f.write_str(ROOT_NAME),
            Self::HostedRoot(client) => write!(f, "{ROOT_NAME}:{}/{}", client.tenant, client.user),
            Self::Node(node) => write!(f, "node:{}", node.name),
            Self::Remote(remote) => write!(f, "remote:{}", remote.peer),
            Self::External(admitter) => write!(f, "external:{admitter}"),
        }
    }
}

/// A session-authenticated web client of a hosted root: the `(tenant,
/// user)` its session verified, set by the hosted root's API from what
/// authenticated the request, never from the request (ADR-0023 § 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedClient {
    pub tenant: String,
    pub user: String,
}

/// Who a hosted broker serves: the `(tenant, user)` of the hosted root
/// conversation it was built for, and what kind of root its runs have
/// (ADR-0021 § 3, ADR-0023 § 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub tenant: String,
    pub user: String,
    pub root: BrokerRoot,
}

/// The root a hosted broker's runs hang from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerRoot {
    /// The user's own hosted root conversation: its client may start,
    /// cancel, read and answer.
    Hosted,
    /// A typed root, for a run a `Remote` or `External` caller started
    /// (ADR-0023 § 2): its client may only cancel and read. `key_owner` is
    /// the admitting key's owner for an `External`-rooted run, who the
    /// client must also be.
    Typed { key_owner: Option<String> },
}

impl Binding {
    /// Whether `client` is the one this broker serves.
    fn admits(&self, client: &HostedClient) -> bool {
        let owner = match &self.root {
            BrokerRoot::Hosted | BrokerRoot::Typed { key_owner: None } => true,
            BrokerRoot::Typed {
                key_owner: Some(owner),
            } => *owner == client.user,
        };
        self.tenant == client.tenant && self.user == client.user && owner
    }
}

/// A node, as the broker knows it: the name its connection was admitted
/// under, the spec it was admitted as, and the runs it serves.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeCaller {
    pub name: String,
    pub spec: String,
    /// Its open runs, never empty: a node with none is chainless.
    pub runs: Vec<OpenRun>,
}

/// One run a node serves, open in the broker's task table.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenRun {
    /// What a call made from it names: the `taskId` of the `task.run` that
    /// opened it (GT-0b).
    pub name: String,
    pub id: RunId,
    /// The chain the broker stamped on it (DP-2).
    pub chain: CallChain,
}

/// An ADR-0022 stream peer. GT-2 gives it its claim result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCaller {
    /// Who is on the other end of the stream.
    pub peer: String,
}

/// Who admitted an `External` caller (ADR-0023 § 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admitter {
    /// A live hive API key at the edge (GT-3).
    Key { id: String, owner: String },
    /// The local MCP gateway's launch token (GT-1).
    LaunchToken,
    /// A broker-made connection with no open run: nothing it asks for is
    /// granted.
    Chainless,
}

impl fmt::Display for Admitter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Key { id, .. } => write!(f, "key:{id}"),
            Self::LaunchToken => f.write_str("launch"),
            Self::Chainless => f.write_str("chainless"),
        }
    }
}

/// A caller's kind, without what it carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallerKind {
    Root,
    Node,
    Remote,
    External,
}

impl CallerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Node => "node",
            Self::Remote => "remote",
            Self::External => "external",
        }
    }
}

/// A request, typed, as the gate reads it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Request<'a> {
    /// `agent.invoke`.
    Invoke(&'a InvokeAgentParams),
    /// `agent.list`.
    List,
    /// `mailbox.post`.
    Post(&'a SendMessageParams),
    /// `mailbox.take` (TM-5): a node takes its mid-run messages.
    Take,
    /// `human.approve`, raised (EN-2a).
    Approve(&'a ApprovalRequest),
    /// The root answers the approval it was handed under `id`. `nonce` is
    /// the one the broker gave the hosted client with it; the local root's
    /// answers carry none.
    AnswerApproval { id: &'a str, nonce: Option<&'a str> },
    /// `human.ask`, raised (EN-2b).
    Ask(&'a AskRequest),
    /// The root answers the question it was handed under `id`; `nonce` as
    /// for [`Request::AnswerApproval`].
    Answer { id: &'a str, nonce: Option<&'a str> },
    /// The local root stops the run `node` names (TB-7).
    Cancel { node: &'a str },
    /// The local root takes the messages waiting for its next run (TM-2).
    TakeRunMessages,
    /// A hosted root conversation's own lifecycle (ADR-0023 § 1: hosted
    /// `Root` rows).
    Conversation(ConversationOp),
}

/// What the hosted client does to a hosted root conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationOp {
    Create,
    List,
    Delete,
}

impl Request<'_> {
    /// The request's method, as a row names it.
    pub fn method(&self) -> Method {
        match self {
            Self::Invoke(_) => Method::Invoke,
            Self::List => Method::List,
            Self::Post(_) => Method::Post,
            Self::Take => Method::Take,
            Self::Approve(_) => Method::Approve,
            Self::AnswerApproval { .. } => Method::AnswerApproval,
            Self::Ask(_) => Method::Ask,
            Self::Answer { .. } => Method::Answer,
            Self::Cancel { .. } => Method::Cancel,
            Self::TakeRunMessages => Method::TakeRunMessages,
            Self::Conversation(ConversationOp::Create) => Method::ConversationCreate,
            Self::Conversation(ConversationOp::List) => Method::ConversationList,
            Self::Conversation(ConversationOp::Delete) => Method::ConversationDelete,
        }
    }
}

/// A request's method, without its params.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Invoke,
    List,
    Post,
    Take,
    Approve,
    AnswerApproval,
    Ask,
    Answer,
    Cancel,
    TakeRunMessages,
    ConversationCreate,
    ConversationList,
    ConversationDelete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Invoke => "agent.invoke",
            Self::List => "agent.list",
            Self::Post => "mailbox.post",
            Self::Take => "mailbox.take",
            Self::Approve => "human.approve",
            Self::AnswerApproval => "root.answer_approval",
            Self::Ask => "human.ask",
            Self::Answer => "root.answer",
            Self::Cancel => "root.cancel",
            Self::TakeRunMessages => "root.take_run_messages",
            Self::ConversationCreate => "conversation.create",
            Self::ConversationList => "conversation.list",
            Self::ConversationDelete => "conversation.delete",
        }
    }
}

/// The row a decision matched: a caller kind and a method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub caller: CallerKind,
    pub method: Method,
}

impl fmt::Display for Row {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.caller.as_str(), self.method.as_str())
    }
}

/// What the gate reads besides the caller and the request: plain data the
/// broker copied out of its registry and runners for this one decision.
pub struct Snapshot<'a> {
    /// The spec rules (PL-S2). Always one: a local broker nobody gave one
    /// consults [`LocalPermissive`](super::LocalPermissive).
    pub policy: &'a dyn CallPolicy,
    pub now: SystemTime,
    /// The id a root call's chain starts from.
    pub root_task_id: String,
    /// Who an `agent.invoke` reaches.
    pub callee: Callee,
    /// The sender's owner, for a `mailbox.post`.
    pub owner: Owner,
    /// The node a `mailbox.post` names, for a mid-run message (TM-5).
    pub addressee: Addressee,
    /// The hosted binding, on a hosted broker; `None` on a local one.
    pub binding: Option<&'a Binding>,
    /// For an answer: the nonce the broker gave the hosted client with
    /// the pending id it names, if one is pending under it.
    pub answer_nonce: Option<String>,
}

/// A snapshot that does not add up — the registry says one thing and the
/// directory another. The gate refuses as an internal error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreadable(pub String);

/// Who an `agent.invoke` reaches, as the broker resolved its name.
#[derive(Debug, Clone, PartialEq)]
pub enum Callee {
    /// The request reaches nobody (not an `agent.invoke`).
    None,
    /// Nobody serves the name.
    Unknown,
    /// A registered participant, by its node name, admitted as `spec`.
    Node { name: String, spec: String },
    /// A virtual agent, which the call spawns a worker of.
    Runner(SpawnView),
}

/// What spawning a virtual agent is decided on.
#[derive(Debug, Clone, PartialEq)]
pub struct SpawnView {
    /// The agent and the settings the root gives it.
    pub target: Target,
    /// The caller's own context (BI-5): a node's as its runner recorded it,
    /// or the root's.
    pub own: SpawnContext,
    /// Whether the context the call brings has its tree inside `own`'s,
    /// resolved on disk ([`spawn_context::lies_inside`]).
    pub inside_own_tree: bool,
}

/// A `mailbox.post` sender's owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner {
    /// The request is not a post, or its sender is not a node.
    None,
    /// The sender is the root's.
    Root,
    /// The sender is the node `name`'s.
    Node {
        id: NodeId,
        name: String,
        ended: bool,
    },
}

/// The node a `mailbox.post` names, as the directory has it (TM-5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Addressee {
    /// The request is not a post, or no node goes by the name it names.
    None,
    Node {
        id: NodeId,
        /// Its owner's name; `None` when the root owns it.
        owner: Option<String>,
        ended: bool,
    },
}

/// What the gate decided, and the row it matched.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub row: Row,
    pub outcome: Result<Grant, Refused>,
}

/// What a granted request may do, and on what.
#[derive(Debug, Clone, PartialEq)]
pub enum Grant {
    /// Run the callee's task on `target`, under `stamp`.
    Invoke {
        target: InvokeTarget,
        stamp: CallStamp,
    },
    List,
    /// Queue the message for `to`, within its list's bounds, for delivery
    /// `at`.
    Post {
        to: PostTo,
        at: Delivery,
    },
    /// Take the caller's mid-run messages (TM-5).
    Take,
    /// Raise the approval with the root, its asker stamped with `chain`:
    /// the chain of the run it was raised from.
    Approve {
        chain: CallChain,
    },
    AnswerApproval,
    /// Raise the question, its asker stamped with `chain`: the chain of the
    /// run it was raised from.
    Ask {
        chain: CallChain,
    },
    Answer,
    Cancel,
    TakeRunMessages,
    /// The hosted client's conversation request; hive performs it.
    Conversation,
}

/// Where a granted `agent.invoke` runs.
#[derive(Debug, Clone, PartialEq)]
pub enum InvokeTarget {
    /// Hand the task to the registered participant `node`.
    Submit { node: String },
    /// Spawn a worker of the virtual agent `runner`, with `context`.
    Spawn {
        runner: String,
        context: SpawnContext,
    },
}

/// Whose pending list a granted message goes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostTo {
    Root,
    Node(NodeId),
}

/// When a granted message reaches its recipient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// At TM-2's delivery points: the recipient's next `invoke_agent`
    /// result, or its next run.
    Run,
    /// At the recipient's next tool round, or else its next run (TM-5).
    ToolRound,
}

/// Why a request was refused. Each keeps the shape that request was
/// refused with before the gate; the new ones are [`CallError::Refused`].
#[derive(Debug, Clone, PartialEq)]
pub enum Refused {
    /// Nobody serves the name ([`CallError::UnknownAgent`]).
    UnknownAgent(String),
    /// The specs or the chain refuse it ([`CallError::Delegation`]).
    Delegation(Refusal),
    /// The callee is not on the caller's roster ([`CallError::Refused`]).
    Roster(String),
    /// The context the call brings reaches outside the caller's
    /// ([`CallError::SpawnContextRefused`]).
    SpawnContext(CallError),
    /// A message the tree does not allow ([`MessageStatus::Refused`](chatty_fabric::MessageStatus)).
    Message(RefusalReason),
    /// No row grants this caller this request ([`CallError::Refused`]).
    Caller(String),
    /// The snapshot did not add up: refused before any effect, and alarmed
    /// ([`CallError::Refused`], `internal: …`).
    Internal(String),
}

impl Refused {
    /// The error a call refused this way ends with.
    pub fn to_call_error(&self) -> CallError {
        match self {
            Self::UnknownAgent(agent) => CallError::UnknownAgent(agent.clone()),
            Self::Delegation(refusal) => CallError::Delegation(refusal.clone()),
            Self::Roster(why) | Self::Caller(why) => CallError::Refused(why.clone()),
            Self::SpawnContext(error) => error.clone(),
            Self::Message(reason) => CallError::Refused(reason.to_string()),
            Self::Internal(why) => CallError::Refused(format!("internal: {why}")),
        }
    }
}

/// Decide `request` from `caller` over `snapshot`, before any effect.
///
/// Pure: no I/O, no lock, no clock, no randomness — everything it reads is
/// in its arguments.
pub fn decide(
    caller: &Caller,
    request: &Request<'_>,
    snapshot: &Result<Snapshot<'_>, Unreadable>,
) -> Decision {
    let row = Row {
        caller: caller.kind(),
        method: request.method(),
    };
    let outcome = match snapshot {
        Ok(snapshot) => decide_row(caller, request, snapshot),
        Err(Unreadable(why)) => Err(Refused::Internal(why.clone())),
    };
    Decision { row, outcome }
}

fn decide_row(
    caller: &Caller,
    request: &Request<'_>,
    snapshot: &Snapshot<'_>,
) -> Result<Grant, Refused> {
    if let (Caller::Root, Some(_)) = (caller, snapshot.binding) {
        return Err(Refused::Caller(format!(
            "{caller} may not {}: a hosted broker's only root is its hosted client",
            request.method().as_str()
        )));
    }
    match (caller, request) {
        (Caller::HostedRoot(client), request) => hosted(caller, client, request, snapshot),
        (Caller::Root | Caller::Node(_), Request::Invoke(params)) => {
            invoke(caller, params, snapshot)
        }
        (Caller::Root | Caller::Node(_), Request::List) => Ok(Grant::List),
        (Caller::Root, Request::Post(_)) => human_post(&snapshot.addressee),
        (Caller::Node(node), Request::Post(params)) => post(node, params, snapshot),
        (Caller::Node(_), Request::Take) => Ok(Grant::Take),
        (Caller::Root, Request::Take) => Err(Refused::Caller(
            "the root's messages open its next run; it takes none mid-run".to_string(),
        )),
        (Caller::Node(node), Request::Approve(_)) => approve(node),
        (Caller::Root, Request::Approve(_)) => Err(Refused::Caller(
            "the root answers approvals; it raises none through its broker".to_string(),
        )),
        (Caller::Root, Request::AnswerApproval { .. }) => Ok(Grant::AnswerApproval),
        (Caller::Node(node), Request::Ask(_)) => ask(node),
        (Caller::Root, Request::Ask(_)) => Err(Refused::Caller(
            "the root answers questions; it raises none through its broker".to_string(),
        )),
        (Caller::Root, Request::Answer { .. }) => Ok(Grant::Answer),
        (Caller::Root, Request::Cancel { .. }) => Ok(Grant::Cancel),
        (Caller::Root, Request::TakeRunMessages) => Ok(Grant::TakeRunMessages),
        (Caller::Root, Request::Conversation(_)) => Err(Refused::Caller(format!(
            "{caller} may not {}: conversations are the hosted root's rows",
            request.method().as_str()
        ))),
        (
            Caller::Node(_),
            Request::AnswerApproval { .. }
            | Request::Answer { .. }
            | Request::Cancel { .. }
            | Request::TakeRunMessages
            | Request::Conversation(_),
        ) => Err(Refused::Caller(format!(
            "{} is the local root's own request",
            request.method().as_str()
        ))),
        // Not on the tree: a message from outside it is refused as one
        // from a node that is not there.
        (Caller::Remote(_) | Caller::External(_), Request::Post(_)) => {
            Err(Refused::Message(RefusalReason::NotOnTree))
        }
        (
            Caller::Remote(_) | Caller::External(_),
            Request::Invoke(_)
            | Request::List
            | Request::Take
            | Request::Approve(_)
            | Request::AnswerApproval { .. }
            | Request::Ask(_)
            | Request::Answer { .. }
            | Request::Cancel { .. }
            | Request::TakeRunMessages
            | Request::Conversation(_),
        ) => Err(Refused::Caller(outside(caller, request))),
    }
}

/// The hosted client's rows (ADR-0023 § 3, "Hosted client"): refused
/// unless its `(tenant, user)` is the broker's binding; on a hosted-root
/// broker it starts, cancels, reads and answers runs and keeps its
/// conversations, and on a typed-root broker it only cancels and reads.
/// An answer names its pending id and that id's nonce, never the session
/// alone.
fn hosted(
    caller: &Caller,
    client: &HostedClient,
    request: &Request<'_>,
    snapshot: &Snapshot<'_>,
) -> Result<Grant, Refused> {
    let method = request.method().as_str();
    let Some(binding) = snapshot.binding else {
        return Err(Refused::Caller(format!(
            "{caller} may not {method}: this broker serves no hosted root"
        )));
    };
    if !binding.admits(client) {
        return Err(Refused::Caller(format!(
            "{caller} may not {method}: this broker serves another tenant or user"
        )));
    }
    let typed = match &binding.root {
        BrokerRoot::Hosted => false,
        BrokerRoot::Typed { .. } => true,
    };
    match request {
        Request::Cancel { .. } => Ok(Grant::Cancel),
        Request::TakeRunMessages => Ok(Grant::TakeRunMessages),
        Request::Invoke(_)
        | Request::List
        | Request::AnswerApproval { .. }
        | Request::Answer { .. }
        | Request::Conversation(_)
            if typed =>
        {
            Err(Refused::Caller(format!(
                "{caller} may not {method}: on a typed-root broker the hosted client only \
                 cancels and reads runs"
            )))
        }
        Request::Invoke(params) => invoke(caller, params, snapshot),
        Request::List => Ok(Grant::List),
        Request::Post(_) => Err(Refused::Message(RefusalReason::NotOnTree)),
        Request::Take => Err(Refused::Caller(format!(
            "{caller} may not {method}: the root takes no mid-run messages"
        ))),
        Request::Approve(_) | Request::Ask(_) => Err(Refused::Caller(format!(
            "{caller} may not {method}: the root answers, it raises none through its broker"
        ))),
        Request::AnswerApproval { id, nonce } => {
            nonce_matches(caller, id, *nonce, snapshot).map(|()| Grant::AnswerApproval)
        }
        Request::Answer { id, nonce } => {
            nonce_matches(caller, id, *nonce, snapshot).map(|()| Grant::Answer)
        }
        Request::Conversation(_) => Ok(Grant::Conversation),
    }
}

/// A hosted answer names a pending id and the nonce the broker handed out
/// with it (ADR-0021 § 2): no nonce, no pending id, or another nonce
/// refuses.
fn nonce_matches(
    caller: &Caller,
    id: &str,
    nonce: Option<&str>,
    snapshot: &Snapshot<'_>,
) -> Result<(), Refused> {
    let Some(nonce) = nonce else {
        return Err(Refused::Caller(format!(
            "{caller} may not answer '{id}' without its nonce: an answer is never session-only"
        )));
    };
    match snapshot.answer_nonce.as_deref() {
        Some(expected) if constant_time_eq(expected.as_bytes(), nonce.as_bytes()) => Ok(()),
        Some(_) | None => Err(Refused::Caller(format!(
            "{caller} may not answer '{id}': no pending request has that id and nonce"
        ))),
    }
}

/// Byte equality whose time does not depend on where the inputs differ.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Why a `Remote` or `External` caller is refused `request`.
fn outside(caller: &Caller, request: &Request<'_>) -> String {
    let why = match caller {
        Caller::External(Admitter::Chainless) => "it serves no open run",
        Caller::External(Admitter::Key { .. }) => "edge rows arrive with GT-3",
        Caller::External(Admitter::LaunchToken) => "gateway rows arrive with GT-1",
        Caller::Remote(_) => "stream rows arrive with GT-2",
        Caller::Root | Caller::HostedRoot(_) | Caller::Node(_) => "no row grants it",
    };
    format!("{caller} may not {}: {why}", request.method().as_str())
}

/// `agent.invoke` for the root or a node: the caller's chain (a node's from
/// the run it names), the callee, the specs, the chain's cycle, depth and
/// budget, then — only when the grant spawns — the roster and the spawn
/// context.
fn invoke(
    caller: &Caller,
    params: &InvokeAgentParams,
    snapshot: &Snapshot<'_>,
) -> Result<Grant, Refused> {
    let (chain, from_run, node) = match caller {
        Caller::Root | Caller::HostedRoot(_) => {
            (CallChain::root(snapshot.root_task_id.clone()), None, None)
        }
        Caller::Node(node) => {
            let run = named_run(node, params.run.as_deref())?;
            (run.chain.clone(), Some(run.id), Some(node))
        }
        Caller::Remote(_) | Caller::External(_) => {
            return Err(Refused::Caller(outside(caller, &Request::Invoke(params))));
        }
    };
    let (callee_spec, spawn) = match &snapshot.callee {
        Callee::Node { spec, .. } => (spec.as_str(), None),
        Callee::Runner(spawn) => (spawn.target.name.as_str(), Some(spawn)),
        Callee::Unknown | Callee::None => return Err(Refused::UnknownAgent(params.agent.clone())),
    };

    let policy = snapshot.policy;
    match node {
        None => policy.root_may_call(callee_spec),
        Some(node) => policy.may_call(&node.spec, callee_spec),
    }
    .map_err(Refused::Delegation)?;
    let own = policy.budget(callee_spec);
    let chain = chain
        .extend(callee_spec)
        .and_then(|chain| chain.budget(&params.remaining, &own, snapshot.now))
        .map_err(Refused::Delegation)?;
    let stamp = CallStamp {
        caller: node.map(|node| node.name.clone()),
        from_run,
        chain,
    };

    let target = match (&snapshot.callee, spawn) {
        (Callee::Node { name, .. }, None) => InvokeTarget::Submit { node: name.clone() },
        (Callee::Runner(_), Some(spawn)) => InvokeTarget::Spawn {
            runner: spawn.target.name.clone(),
            context: spawn_from(caller, params, spawn)?,
        },
        (Callee::Node { .. } | Callee::Runner(_) | Callee::Unknown | Callee::None, _) => {
            return Err(Refused::UnknownAgent(params.agent.clone()));
        }
    };
    Ok(Grant::Invoke { target, stamp })
}

/// The run a node's call is made from: the one it names, which must be one
/// of its own open runs (GT-0b).
fn named_run<'n>(node: &'n NodeCaller, named: Option<&str>) -> Result<&'n OpenRun, Refused> {
    let Some(named) = named else {
        return Err(Refused::Caller(format!(
            "{}'s agent.invoke names no run: a node calls from the run it serves",
            node.name
        )));
    };
    node.runs
        .iter()
        .find(|run| run.name == named)
        .ok_or_else(|| {
            Refused::Caller(format!(
                "run '{named}' is not one of {}'s open runs",
                node.name
            ))
        })
}

/// The context a spawned worker starts from: the caller's own, derived or
/// clamped (BI-5). A target outside the caller's roster is refused: a
/// sub-leader reaches only what it was given.
fn spawn_from(
    caller: &Caller,
    params: &InvokeAgentParams,
    spawn: &SpawnView,
) -> Result<SpawnContext, Refused> {
    if !spawn.own.roster.contains(&spawn.target.name) {
        let name = match caller {
            Caller::Node(node) => node.name.clone(),
            Caller::Root | Caller::HostedRoot(_) | Caller::Remote(_) | Caller::External(_) => {
                caller.to_string()
            }
        };
        return Err(Refused::Roster(format!(
            "'{}' is not on {name}'s roster",
            spawn.target.name
        )));
    }
    match params.spawn_context.clone() {
        None => Ok(spawn_context::derive(&spawn.own, &spawn.target)),
        Some(requested) => {
            spawn_context::clamp(*requested, &spawn.own, &spawn.target, spawn.inside_own_tree)
                .map_err(Refused::SpawnContext)
        }
    }
}

/// `human.ask` from a node: raised from the task it serves, whose run's
/// chain stamps the asker (EN-2b). Like `human.approve`, it names no run,
/// so a node serving more than one is refused rather than guessed for.
fn ask(node: &NodeCaller) -> Result<Grant, Refused> {
    match &node.runs[..] {
        [run] => Ok(Grant::Ask {
            chain: run.chain.clone(),
        }),
        runs => Err(Refused::Caller(format!(
            "{} serves {} runs, and its human.ask names none",
            node.name,
            runs.len()
        ))),
    }
}

/// `human.approve` from a node: raised from the task it serves, whose
/// run's chain stamps the asker (EN-2a). The request names no run, so a
/// node serving more than one is refused rather than guessed for.
fn approve(node: &NodeCaller) -> Result<Grant, Refused> {
    match &node.runs[..] {
        [run] => Ok(Grant::Approve {
            chain: run.chain.clone(),
        }),
        runs => Err(Refused::Caller(format!(
            "{} serves {} runs, and its human.approve names none",
            node.name,
            runs.len()
        ))),
    }
}

/// `mailbox.post` from a node: to its owner, while the owner is live, at
/// TM-2's delivery points (TM-1); or to its own live child, at the child's
/// next tool round (TM-5). Anyone else — a sibling, itself, a grandchild,
/// a name nobody has — is not on the tree. The list's bounds are the
/// effect's quota.
fn post(node: &NodeCaller, params: &SendMessageParams, snapshot: &Snapshot<'_>) -> Result<Grant, Refused> {
    let owner = match &snapshot.owner {
        Owner::Root => Some((PostTo::Root, ROOT_NAME, false)),
        Owner::Node { id, name, ended } => Some((PostTo::Node(*id), name.as_str(), *ended)),
        Owner::None => None,
    };
    if let Some((to, name, ended)) = owner
        && params.to == name
    {
        if ended {
            return Err(Refused::Message(RefusalReason::RecipientEnded));
        }
        return Ok(Grant::Post {
            to,
            at: Delivery::Run,
        });
    }
    match &snapshot.addressee {
        Addressee::Node {
            id,
            owner: Some(owner),
            ended,
        } if *owner == node.name => mid_run(*id, *ended),
        Addressee::Node { .. } | Addressee::None => {
            Err(Refused::Message(RefusalReason::NotOnTree))
        }
    }
}

/// `mailbox.post` from the human, through the local root: to any live node
/// of its broker, at that node's next tool round (TM-5).
fn human_post(addressee: &Addressee) -> Result<Grant, Refused> {
    match addressee {
        Addressee::Node { id, ended, .. } => mid_run(*id, *ended),
        Addressee::None => Err(Refused::Message(RefusalReason::NotOnTree)),
    }
}

/// A mid-run message to the node `id`, unless it has ended.
fn mid_run(id: NodeId, ended: bool) -> Result<Grant, Refused> {
    if ended {
        return Err(Refused::Message(RefusalReason::RecipientEnded));
    }
    Ok(Grant::Post {
        to: PostTo::Node(id),
        at: Delivery::ToolRound,
    })
}

#[cfg(test)]
#[path = "gate_tests.rs"]
mod tests;
