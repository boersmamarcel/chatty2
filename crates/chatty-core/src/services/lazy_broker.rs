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
//! and [`ListAgentsTool`](crate::tools::list_agents_tool::ListAgentsTool) ask
//! it for its direct [`Transport`](LazyBroker::transport) right before they
//! need the broker. The first call actually starts it; every later call is a
//! cheap, memoized no-op that returns the same handle.
//!
//! `bound_sockets` lets a test observe whether the broker has actually bound
//! its sockets, without triggering a start itself.

use async_trait::async_trait;
use chatty_fabric::{
    CallError, CallEvent, CallRequest, CallResult, MessageStatus, SendMessageParams, Transport,
};
use futures::StreamExt;
use std::path::PathBuf;
use std::sync::Arc;

/// See the module docs.
#[async_trait]
pub trait LazyBroker: Send + Sync {
    /// Starts the broker if it has not started yet, and returns a direct
    /// [`Transport`] into it: how the in-process root reaches its local
    /// roles without a socket or an HTTP hop (ADR-0020, BI-4). `Ok(None)`
    /// — the default — means this broker offers none. An error here means
    /// the broker is configured but could not start (e.g. its gateway
    /// socket already in use, AGE-746); `ListAgentsTool` surfaces it in its
    /// `note` rather than treating it the same as "no broker configured".
    async fn transport(&self) -> anyhow::Result<Option<Arc<dyn Transport>>> {
        Ok(None)
    }

    /// The sockets this broker is serving on right now: empty before the
    /// first successful [`transport`](Self::transport) call. It never opens
    /// a TCP port (ADR-0021 § 4).
    fn bound_sockets(&self) -> Vec<PathBuf>;

    /// The root's next run is starting: take the tree messages waiting for
    /// it (TM-2), wrapped as untrusted data and oldest first, to open its
    /// user turn with ([`Transport::take_run_messages`]). Never starts the
    /// broker — one that has not started has no messages — so the default,
    /// for a broker with no direct transport, is none.
    fn take_run_messages(&self) -> Vec<String> {
        Vec::new()
    }

    /// Stop `node` and everything under it while the rest of the swarm
    /// keeps running (TB-7, AGE-749); see [`Transport::cancel`]. Never
    /// starts the broker: one that has not started runs nothing to stop.
    fn cancel(&self, node: &str) -> anyhow::Result<()> {
        anyhow::bail!("no broker is running '{node}'")
    }

    /// Send `text` from the human to the running agent `to` (TM-5): it
    /// reaches that agent at its next tool round, wrapped as untrusted
    /// data, and grants it nothing. Never starts the broker: one that has
    /// not started runs nobody to message.
    async fn post_message(&self, to: &str, _text: String) -> anyhow::Result<MessageStatus> {
        anyhow::bail!("no broker is running '{to}'")
    }

    /// Stop serving, if this ever started. A no-op otherwise (nothing to
    /// stop) — the default for a host whose lifecycle already tears the
    /// underlying gateway down some other way.
    fn shutdown(&self) {}
}

/// `text` from the human, posted to `to` through the root's direct
/// `transport` (TM-5): what [`LazyBroker::post_message`] does once the
/// broker has started.
pub async fn post_as_root(
    transport: &dyn Transport,
    to: &str,
    text: String,
) -> Result<MessageStatus, CallError> {
    let mut stream = transport
        .call(CallRequest::SendMessage(SendMessageParams {
            to: to.to_string(),
            text,
        }))
        .await?;
    while let Some(event) = stream.next().await {
        if let CallEvent::Result(CallResult::Posted(status)) = event? {
            return Ok(status);
        }
    }
    Err(CallError::Failed(
        "the broker answered mailbox.post with no status".to_string(),
    ))
}
