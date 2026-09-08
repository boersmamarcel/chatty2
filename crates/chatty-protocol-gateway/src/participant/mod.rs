//! Local participants: processes that register over a socket and are served
//! as A2A agents by the same gateway that serves WASM modules.
//!
//! ADR-0011 routes all fleet coordination through one broker. This is the
//! local half of it: a child process connects to a Unix socket, publishes an
//! agent card, and from then on is addressable at `/a2a/{name}` exactly like
//! a module — same JSON-RPC methods, same SSE shape, so the existing
//! `A2aClient` reaches it unchanged.
//!
//! The pieces:
//!
//! * [`budget`] — how many workers may hold one model endpoint at a time.
//! * [`protocol`] — the frames on the socket. Newline-delimited JSON, not
//!   A2A: A2A is the broker's public format, a child process is not public.
//! * [`registry`] — who is registered and where each open task's updates go.
//! * [`listener`] — the accept loop, and the rule that a closed socket
//!   deregisters its participant and fails its open tasks.
//! * [`client`] — the other end of the socket, which a chatty child speaks.
//! * [`virtual_agent`] — the interface a runner implements: an agent with no
//!   participant until a task arrives.
//! * [`runner`] — spawning a chatty child and exposing it as a participant
//!   (ADR-0011 C2).

mod budget;
mod protocol;
mod registry;
mod virtual_agent;

pub use budget::{DEFAULT_ENDPOINT_LIMIT, EndpointBudget, EndpointPermit};
pub use protocol::{BrokerFrame, ParticipantCard, ParticipantFrame, ParticipantSkill, TaskState};
pub use registry::{ParticipantRegistry, RegisterError, TaskStream, TaskUpdate};
pub use virtual_agent::{VirtualAgent, WorkerFuture, WorkerHandle};

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
pub use client::ParticipantConnection;
#[cfg(unix)]
pub use listener::{bind, serve, serve_connection, unbind};
#[cfg(unix)]
pub use runner::{LocalRunner, Worker, WorkerWorkspace, WorkspaceFactory};
