//! Local participants: worker processes the broker spawns and serves as A2A
//! agents from the same gateway that serves WASM modules.
//!
//! ADR-0011 routes all fleet coordination through one broker. This is the
//! local half of it: the broker admits a node, makes a socket pair for it
//! and hands one end to the child it spawns (ADR-0020). The child says
//! `hello` with an agent card, the broker answers `welcome` with the name it
//! chose, and from then on the child is addressable at `/a2a/{name}` exactly
//! like a module — same JSON-RPC methods, same SSE shape, so the existing
//! `A2aClient` reaches it unchanged.
//!
//! The pieces:
//!
//! * [`budget`] — how many workers may hold one model endpoint at a time.
//! * [`protocol`] — the frames on the socket. Newline-delimited JSON, not
//!   A2A: A2A is the broker's public format, a child process is not public.
//! * [`registry`] — who is registered and where each open task's updates go,
//!   and the way back down to a task parked on a question (AGE-306).
//! * [`calls`] — the calls a worker makes over its connection, and the
//!   root's direct handle, run as the node that made them (BI-4).
//! * [`listener`] — broker-made connections, the rule that a closed one
//!   deregisters its participant and fails its open tasks, and the shared
//!   socket that refuses every registration.
//! * [`client`] — the other end of the socket, which a chatty child speaks.
//! * [`virtual_agent`] — the interface a runner implements: an agent with no
//!   participant until a task arrives.
//! * [`runner`] — spawning a chatty child and exposing it as a participant
//!   (ADR-0011 C2).

mod budget;
mod calls;
mod protocol;
mod registry;
mod virtual_agent;

pub use budget::{DEFAULT_ENDPOINT_LIMIT, EndpointBudget, EndpointPermit};
pub use calls::{BrokerCalls, Caller, DirectTransport};
pub use protocol::{
    BrokerFrame, CALLER_ENV, CALLER_HEADER, DelegatedTask, FrameError, InputAnswer, InputQuestion,
    InputRequest, PROTOCOL_VERSION, ParticipantCard, ParticipantFrame, ParticipantSkill,
    TaskBearer, TaskInput, TaskState, decode_frame, encode_frame,
};
pub use registry::{
    AdmittedNode, AnswerError, ParticipantRegistry, ROOT_SCOPE, RegisteredAgent, TaskStream,
    TaskUpdate,
};
pub use virtual_agent::{EvidenceFuture, TaskEvidence, VirtualAgent, WorkerFuture, WorkerHandle};

// The socket itself is Unix-only. Everything above it is not, so the
// registry and the frames still compile (and are still tested) elsewhere;
// only the transport is gated. The hosted transport is vsock (AGE-307).
#[cfg(unix)]
mod client;
#[cfg(unix)]
mod listener;
#[cfg(unix)]
mod runner;

#[cfg(unix)]
pub use client::{ParticipantConnection, ParticipantReader, ParticipantWriter};
#[cfg(unix)]
pub use listener::{LocalConnection, bind, open_connection, serve, serve_connection, unbind};
#[cfg(unix)]
pub use runner::{
    EvidenceFactory, LocalRunner, PARTICIPANT_FD, Worker, WorkerWorkspace, WorkspaceFactory,
};
