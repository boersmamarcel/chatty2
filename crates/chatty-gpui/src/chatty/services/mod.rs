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
