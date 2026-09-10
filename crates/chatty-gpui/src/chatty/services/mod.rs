// Re-export everything from chatty-core services (types + submodules)
pub use chatty_core::services::*;

/// Broker wiring: the participant socket and the `local-agent` runner
/// (ADR-0011 C2 / AGE-301). Unix-only: it binds a Unix socket, and the
/// gateway's `LocalRunner` behind it is `#[cfg(unix)]`.
#[cfg(unix)]
pub mod broker_runner;
