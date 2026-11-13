//! The one metering path for module calls (AGE-837): every call to an
//! installed Hive module, whether the desktop's MCP gateway serves it or an
//! agent spec loaded it as a plugin, is admitted by [`ModuleMeter::admit`]
//! before it runs.
//!
//! Paid artifacts are hosted-only (ADR-0024), so the desktop runs free
//! modules only: a paid module is always refused and a free one runs.

use std::collections::HashSet;

/// What one finished call cost, as the runtime measured it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CallUsage {
    pub input_tokens: Option<i32>,
    pub output_tokens: Option<i32>,
    pub fuel_consumed: Option<u64>,
    pub execution_ms: Option<u32>,
}

/// Admits module calls: which modules are paid, and so refused.
#[derive(Default)]
pub struct ModuleMeter {
    paid: HashSet<String>,
}

impl std::fmt::Debug for ModuleMeter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModuleMeter")
            .field("paid", &self.paid)
            .finish()
    }
}

impl ModuleMeter {
    /// A meter for which `paid` are the paid modules; it refuses every call
    /// to them.
    pub fn new(paid: HashSet<String>) -> Self {
        Self { paid }
    }

    /// Whether `module` is a paid module.
    pub fn is_paid(&self, module: &str) -> bool {
        self.paid.contains(module)
    }

    /// Whether the call to `module` may run: always for a free module; a
    /// paid module is refused, because paid artifacts run hosted only
    /// (ADR-0024). The error is the reason, as the caller shows it.
    pub async fn admit(&self, module: &str) -> Result<(), String> {
        if self.is_paid(module) {
            return Err("paid modules run hosted".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meter() -> ModuleMeter {
        ModuleMeter::new(HashSet::from(["paid-mod".to_string()]))
    }

    #[tokio::test]
    async fn paid_module_is_refused_on_desktop() {
        let refused = meter().admit("paid-mod").await;
        assert_eq!(refused.unwrap_err(), "paid modules run hosted");
    }

    #[tokio::test]
    async fn free_module_still_runs() {
        assert!(meter().admit("free-mod").await.is_ok());
    }
}
