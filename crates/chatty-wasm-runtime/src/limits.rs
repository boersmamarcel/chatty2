/// Resource limits applied to every WASM module instance.
///
/// These caps prevent a misbehaving module from consuming unbounded host
/// resources (CPU fuel, memory, wall-clock time).
#[derive(Debug, Clone)]
pub struct ResourceLimits {
    /// Maximum number of Wasmtime fuel units the module may consume.
    ///
    /// Wasmtime burns one unit per Wasm instruction (approximately). The
    /// default of 1 000 000 allows a few hundred thousand typical operations
    /// before the engine traps with a clean "fuel exhausted" error.
    pub max_fuel: u64,

    /// Maximum linear-memory size the module may allocate, in bytes.
    ///
    /// Defaults to 64 MiB.
    pub max_memory_bytes: u64,

    /// Wall-clock execution timeout in milliseconds.
    ///
    /// If a call to any guest export takes longer than this, the async
    /// wrapper in [`WasmModule`] returns a timeout error.
    ///
    /// Defaults to 30 000 ms (30 seconds).
    pub max_execution_ms: u64,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_fuel: 100_000_000,
            max_memory_bytes: 64 * 1024 * 1024, // 64 MiB
            max_execution_ms: 300_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_limits_are_sane() {
        let limits = ResourceLimits::default();
        assert_eq!(limits.max_fuel, 100_000_000);
        assert_eq!(limits.max_memory_bytes, 64 * 1024 * 1024);
        assert_eq!(limits.max_execution_ms, 300_000);
    }

    #[test]
    fn limits_are_cloneable_and_debug() {
        let limits = ResourceLimits {
            max_fuel: 500_000,
            max_memory_bytes: 32 * 1024 * 1024,
            max_execution_ms: 5_000,
        };
        let cloned = limits.clone();
        assert_eq!(cloned.max_fuel, 500_000);
        assert_eq!(cloned.max_memory_bytes, 32 * 1024 * 1024);
        assert_eq!(cloned.max_execution_ms, 5_000);

        // Verify Debug impl exists
        let debug_str = format!("{:?}", cloned);
        assert!(debug_str.contains("500000"));
    }
}
