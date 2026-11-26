// Re-export everything from chatty-core services (types + submodules)
pub use chatty_core::services::*;

/// Broker wiring: the participant socket and the `local-agent` runner
/// (ADR-0011 C2 / AGE-301). Unix-only: it binds a Unix socket, and the
/// gateway's `LocalRunner` behind it is `#[cfg(unix)]`.
#[cfg(unix)]
pub mod broker_runner;
/// AGE-744: the desktop's root conversation delegates through its broker.
#[cfg(all(test, unix))]
mod desktop_delegation_tests;
/// The desktop's `LazyBroker` (BI-2, AGE-634). Cross-platform, unlike
/// `broker_runner`: the module gateway it starts lazily is not Unix-only,
/// only the fleet-broker socket riding on it is.
pub mod lazy_gateway_broker;
/// OS notifications for cloud conversations (HX-12).
pub mod os_notification;

/// Whether a configured MCP or A2A `url` points at this machine, for the
/// "External" badges. A plain substring test, as every caller had it.
pub fn url_is_local(url: &str) -> bool {
    url.contains("localhost") || url.contains("127.0.0.1")
}
