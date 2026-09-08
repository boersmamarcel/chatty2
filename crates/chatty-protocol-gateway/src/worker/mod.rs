//! The worker half of the participant socket: being a participant, not
//! serving one.
//!
//! [`super::participant`] is the broker's side — who is registered, and how a
//! caller's A2A request reaches them. This module is the other end of the
//! same socket: the loop a process runs when it *is* the worker, and the
//! table that turns its turn into task frames.
//!
//! # Why it lives in this crate
//!
//! Two processes can be a worker: `chatty-tui --participant-socket` on the
//! desktop (ADR-0011 C2) and hive's `chatty-server` inside a microVM
//! (AGE-307, C8). The sequence of frames a parent renders has to be the same
//! from both — ADR-0011's first kill criterion is measured by diffing exactly
//! that sequence, so a second copy of [`TaskMapper`] would be a second answer
//! to the question the ADR asks. Putting it beside the broker's own side of
//! the protocol also means the two halves cannot drift, which is the reason
//! [`super::participant::client`](super::participant) is here rather than in
//! `chatty-tui`.
//!
//! It is behind the `worker` feature because it is the only thing in this
//! crate that needs `chatty-core`: a broker that never runs an agent itself —
//! the hosted one — builds without it.

mod mapper;

pub use mapper::TaskMapper;

#[cfg(unix)]
mod one_task;

#[cfg(unix)]
pub use one_task::{EventSink, serve_one_task, worker_card};
