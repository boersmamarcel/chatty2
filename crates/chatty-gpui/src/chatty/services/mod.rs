// Re-export everything from chatty-core services (types + submodules)
pub use chatty_core::services::*;

/// Broker wiring: the participant socket and the `local-agent` runner
/// (ADR-0011 C2 / AGE-301).
pub mod broker_runner;
