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
