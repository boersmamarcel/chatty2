//! Resource limits for WASM module calls, and the host ceilings they are
//! clamped to.
//!
//! The ceilings are the host's hard caps (PL-D3, decided 2026-09-27). The
//! defaults *are* the ceilings; a module manifest may only lower a limit,
//! never raise it: the module registry clamps every manifest-derived limit
//! with [`ResourceLimits::clamped`]. An embedder that builds a `WasmModule`
//! directly passes its limits as given.

use std::time::Duration;

// ---------------------------------------------------------------------------
// Host ceilings (PL-D3) — the one place these numbers live.
// ---------------------------------------------------------------------------

/// Linear-memory ceiling for one module instance: 256 MiB.
pub const MAX_MEMORY_BYTES_CEILING: u64 = 256 * 1024 * 1024;

/// Wall-clock ceiling for one guest call, host time included: 60 s.
pub const MAX_EXECUTION_MS_CEILING: u64 = 60_000;

/// Fuel ceiling for one guest call: 10⁹ units (about one per Wasm instruction).
pub const MAX_FUEL_CEILING: u64 = 1_000_000_000;

/// Ceiling on the size of one export's return value: 1 MiB.
pub const MAX_OUTPUT_BYTES_CEILING: u64 = 1024 * 1024;

/// Wall-clock budget for the metadata exports (`list-tools`,
/// `get-agent-card`): 1 s, or the call limit if that is lower.
pub const METADATA_CALL_MS: u64 = 1_000;

/// How often the process-wide ticker advances every engine's epoch. The
/// wall-clock deadline is enforced at this granularity.
pub const EPOCH_TICK: Duration = Duration::from_millis(10);

/// Resource limits applied to every call into a WASM module instance.
///
/// Every limit is per call: fuel and the wall-clock deadline are reset before
/// each export call, so a long-lived module never runs out of a lifetime
/// budget.
#[derive(Debug, Clone)]
pub struct ResourceLimits {
    /// Wasmtime fuel units one call may consume (about one per Wasm
    /// instruction). Exhausting it fails the call with `fuel exhausted`.
    ///
    /// Defaults to [`MAX_FUEL_CEILING`] (10⁹).
    pub max_fuel: u64,

    /// Maximum linear-memory size the instance may grow to, in bytes. A
    /// guest that dies after the limiter refused a grow fails with
    /// `memory limit`.
    ///
    /// Defaults to [`MAX_MEMORY_BYTES_CEILING`] (256 MiB).
    pub max_memory_bytes: u64,

    /// Wall-clock limit for one `chat` / `invoke-tool` call, in
    /// milliseconds, including time spent in host imports (`llm::complete`,
    /// `file::read-bytes`, billing). Enforced by epoch interruption, so it
    /// fires even when the guest never yields; the call fails with
    /// `deadline exceeded`.
    ///
    /// Defaults to [`MAX_EXECUTION_MS_CEILING`] (60 s).
    pub max_execution_ms: u64,

    /// Maximum size of one export's return value, in bytes. A larger result
    /// fails with `output too large`.
    ///
    /// Defaults to [`MAX_OUTPUT_BYTES_CEILING`] (1 MiB).
    pub max_output_bytes: u64,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_fuel: MAX_FUEL_CEILING,
            max_memory_bytes: MAX_MEMORY_BYTES_CEILING,
            max_execution_ms: MAX_EXECUTION_MS_CEILING,
            max_output_bytes: MAX_OUTPUT_BYTES_CEILING,
        }
    }
}

impl ResourceLimits {
    /// These limits with every value above its host ceiling lowered to the
    /// ceiling. Values at or below a ceiling are kept.
    pub fn clamped(self) -> Self {
        Self {
            max_fuel: self.max_fuel.min(MAX_FUEL_CEILING),
            max_memory_bytes: self.max_memory_bytes.min(MAX_MEMORY_BYTES_CEILING),
            max_execution_ms: self.max_execution_ms.min(MAX_EXECUTION_MS_CEILING),
            max_output_bytes: self.max_output_bytes.min(MAX_OUTPUT_BYTES_CEILING),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_ceilings() {
        let limits = ResourceLimits::default();
        assert_eq!(limits.max_fuel, 1_000_000_000);
        assert_eq!(limits.max_memory_bytes, 256 * 1024 * 1024);
        assert_eq!(limits.max_execution_ms, 60_000);
        assert_eq!(limits.max_output_bytes, 1024 * 1024);
    }

    #[test]
    fn limits_are_clamped_to_ceilings() {
        let above = ResourceLimits {
            max_fuel: u64::MAX,
            max_memory_bytes: u64::MAX,
            max_execution_ms: u64::MAX,
            max_output_bytes: u64::MAX,
        }
        .clamped();
        assert_eq!(above.max_fuel, MAX_FUEL_CEILING);
        assert_eq!(above.max_memory_bytes, MAX_MEMORY_BYTES_CEILING);
        assert_eq!(above.max_execution_ms, MAX_EXECUTION_MS_CEILING);
        assert_eq!(above.max_output_bytes, MAX_OUTPUT_BYTES_CEILING);

        // Lower values are kept: a limit may only be lowered.
        let below = ResourceLimits {
            max_fuel: 500_000,
            max_memory_bytes: 32 * 1024 * 1024,
            max_execution_ms: 5_000,
            max_output_bytes: 4096,
        }
        .clamped();
        assert_eq!(below.max_fuel, 500_000);
        assert_eq!(below.max_memory_bytes, 32 * 1024 * 1024);
        assert_eq!(below.max_execution_ms, 5_000);
        assert_eq!(below.max_output_bytes, 4096);
    }
}
