//! Why a guest call failed.
//!
//! [`WasmModule`](crate::WasmModule)'s export wrappers return `anyhow::Error`;
//! when a limit or a trap ended the call, that error is a [`CallError`]
//! (reach it with `err.downcast_ref::<CallError>()`). Each variant's
//! `Display` starts with a fixed phrase callers and tests match on.

use std::fmt;

/// A guest call ended by a resource limit or a trap, not by the guest
/// returning its own `Err`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// The call used up its per-call fuel budget.
    FuelExhausted { max_fuel: u64 },
    /// The call ran past its wall-clock deadline (guest or host time).
    DeadlineExceeded { max_execution_ms: u64 },
    /// The guest died after the memory limiter refused to grow its memory.
    MemoryLimit { max_memory_bytes: u64 },
    /// The guest trapped (a panic, `unreachable`, …). Carries wasmtime's
    /// trap message and the tail of the guest's stderr (a panic message).
    GuestTrap(String),
    /// The export's return value was bigger than the output cap.
    OutputTooLarge { bytes: usize, max_output_bytes: u64 },
    /// The host thread running the call panicked (a host bug, not the guest).
    HostPanic(String),
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FuelExhausted { max_fuel } => {
                write!(
                    f,
                    "fuel exhausted: the call used its {max_fuel}-unit budget"
                )
            }
            Self::DeadlineExceeded { max_execution_ms } => write!(
                f,
                "deadline exceeded: the call timed out after {max_execution_ms} ms"
            ),
            Self::MemoryLimit { max_memory_bytes } => write!(
                f,
                "memory limit: the guest needed more than {max_memory_bytes} bytes"
            ),
            Self::GuestTrap(message) => write!(f, "guest trap: {message}"),
            Self::OutputTooLarge {
                bytes,
                max_output_bytes,
            } => write!(
                f,
                "output too large: {bytes} bytes exceeds the {max_output_bytes}-byte cap"
            ),
            Self::HostPanic(message) => write!(f, "host panic during guest call: {message}"),
        }
    }
}

impl std::error::Error for CallError {}

use crate::bindings::exports::chatty::plugin::plugin::{ToolError, ToolErrorKind};

impl ToolErrorKind {
    /// The kind's WIT name (`unknown-tool`, `invalid-arguments`, …).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownTool => "unknown-tool",
            Self::InvalidArguments => "invalid-arguments",
            Self::Denied => "denied",
            Self::Failed => "failed",
        }
    }
}

/// A tool call the guest itself failed (its `tool-error`). Reach it from
/// [`WasmModule::invoke_tool`](crate::WasmModule::invoke_tool)'s error with
/// `err.downcast_ref::<ToolFailure>()`. `Display` is `<kind>: <message>`,
/// which is what a model reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolFailure {
    pub kind: ToolErrorKind,
    pub message: String,
}

impl From<ToolError> for ToolFailure {
    fn from(error: ToolError) -> Self {
        Self {
            kind: error.kind,
            message: error.message,
        }
    }
}

impl fmt::Display for ToolFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.as_str(), self.message)
    }
}

impl std::error::Error for ToolFailure {}
