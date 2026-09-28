//! A local broker/gateway a host has not necessarily started yet (BI-2,
//! AGE-634).
//!
//! Before this, the desktop and chatty-tui started their broker — the
//! protocol gateway plus the participant socket and local worker runners
//! behind it (ADR-0011 C2) — as soon as settings/models were ready, whether
//! or not a conversation ever delegated to it. That opened a loopback TCP
//! listener nobody had asked for yet.
//!
//! Now each host builds a [`LazyBroker`] instead: something that knows how
//! to start the broker, but has not. [`InvokeAgentTool`](crate::tools::invoke_agent_tool::InvokeAgentTool)
//! and [`ListAgentsTool`](crate::tools::list_agents_tool::ListAgentsTool) call
//! [`LazyBroker::ensure_started`] right before they would otherwise reach for
//! a pre-resolved `gateway_base_url`. The first call actually starts it; every
//! later call is a cheap, memoized no-op that returns the same base URL.
//!
//! `bound_addrs` lets a test observe whether the broker has actually bound a
//! TCP listener, without triggering a start itself.

use async_trait::async_trait;
use chatty_fabric::Transport;
use std::net::SocketAddr;
use std::sync::Arc;

/// See the module docs.
#[async_trait]
pub trait LazyBroker: Send + Sync {
    /// Starts the broker if it has not started yet, and returns its loopback
    /// base URL (`http://localhost:<port>`) either way. An error here means
    /// the broker could not start; callers treat that exactly as they treat
    /// "no gateway configured".
    async fn ensure_started(&self) -> anyhow::Result<String>;

    /// Starts the broker if it has not started yet, and returns a direct
    /// [`Transport`] into it: how the in-process root reaches its local
    /// roles without a socket or an HTTP hop (ADR-0020, BI-4). `Ok(None)`
    /// — the default — means this broker offers none, and callers keep
    /// reaching it over [`ensure_started`](Self::ensure_started)'s URL.
    async fn transport(&self) -> anyhow::Result<Option<Arc<dyn Transport>>> {
        Ok(None)
    }

    /// The TCP addresses this broker is bound to right now: empty before the
    /// first successful [`ensure_started`](Self::ensure_started) call, and
    /// always empty for a run that never opens one (e.g. the module gateway
    /// disabled, or `--broker` never passed).
    fn bound_addrs(&self) -> Vec<SocketAddr>;

    /// Stop serving, if this ever started. A no-op otherwise (nothing to
    /// stop) — the default for a host whose lifecycle already tears the
    /// underlying gateway down some other way.
    fn shutdown(&self) {}
}
