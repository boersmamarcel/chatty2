//! The one metering path for module calls (AGE-837): every call to an
//! installed Hive module, whether the desktop's MCP gateway serves it or an
//! agent spec loaded it as a plugin, is admitted by [`ModuleMeter::admit`]
//! before it runs and reported by [`ModuleMeter::record`] after it answers.
//!
//! A paid module needs both halves: a [`CreditGuard`] to admit the call and
//! a [`UsageCollector`] to report it (its reporting is
//! [`ReportingPolicy::Required`](crate::usage::ReportingPolicy::Required)).
//! A meter without them refuses every paid call rather than run it unbilled.
//! Free modules always run; their calls are reported when a collector is
//! attached (analytics, opt-out).

use std::collections::HashSet;
use std::sync::Arc;

use crate::credit_guard::{Admission, CreditGuard};
use crate::usage::UsageCollector;

/// What one finished call cost, as the runtime measured it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CallUsage {
    pub input_tokens: Option<i32>,
    pub output_tokens: Option<i32>,
    pub fuel_consumed: Option<u64>,
    pub execution_ms: Option<u32>,
}

/// Admits and reports module calls: which modules are paid, and the guard
/// and collector that bill them.
#[derive(Default)]
pub struct ModuleMeter {
    paid: HashSet<String>,
    guard: Option<Arc<CreditGuard>>,
    usage: Option<Arc<UsageCollector>>,
    flush_on_record: bool,
}

impl std::fmt::Debug for ModuleMeter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModuleMeter")
            .field("paid", &self.paid)
            .field("guard", &self.guard.is_some())
            .field("usage", &self.usage.is_some())
            .field("flush_on_record", &self.flush_on_record)
            .finish()
    }
}

impl ModuleMeter {
    /// A meter for which `paid` are the paid modules. With no guard and
    /// collector attached it refuses every call to them.
    pub fn new(paid: HashSet<String>) -> Self {
        Self {
            paid,
            ..Self::default()
        }
    }

    /// Admit paid calls through `guard`.
    pub fn with_credit_guard(mut self, guard: Arc<CreditGuard>) -> Self {
        self.guard = Some(guard);
        self
    }

    /// Report calls to `usage`.
    pub fn with_usage_collector(mut self, usage: Arc<UsageCollector>) -> Self {
        self.usage = Some(usage);
        self
    }

    /// Flush the collector after every recorded paid call, for a process
    /// too short-lived to wait for the background flush (a delegated worker).
    /// What does not go out stays in the offline queue the next flush reads.
    pub fn flushing_each_call(mut self) -> Self {
        self.flush_on_record = true;
        self
    }

    /// Whether `module` is a paid module.
    pub fn is_paid(&self, module: &str) -> bool {
        self.paid.contains(module)
    }

    /// Whether the call to `module` may run: always for a free module; for
    /// a paid one only with a collector to report it and the guard's
    /// admission (free tier, then credits). The error is the reason, as the
    /// caller shows it.
    pub async fn admit(&self, module: &str) -> Result<Option<Admission>, String> {
        if !self.is_paid(module) {
            return Ok(None);
        }
        if self.usage.is_none() {
            return Err(format!(
                "usage reporting is required for paid module '{module}' but no usage collector \
                 is configured"
            ));
        }
        let Some(guard) = self.guard.as_ref() else {
            return Err(format!(
                "paid module '{module}' needs a credit check, and none is configured: sign in \
                 to Hive in chatty"
            ));
        };
        guard.admit(module).await.map(Some).map_err(|e| e.to_string())
    }

    /// Report one successful call to `module` at `version`, if a collector
    /// is attached.
    pub async fn record(&self, module: &str, version: &str, usage: CallUsage) {
        let Some(collector) = self.usage.as_ref() else {
            return;
        };
        collector
            .record_invocation(
                module,
                version,
                usage.input_tokens,
                usage.output_tokens,
                usage.fuel_consumed,
                usage.execution_ms,
            )
            .await;
        if self.flush_on_record
            && self.is_paid(module)
            && let Err(e) = collector.flush().await
        {
            tracing::warn!(module, error = %e, "usage flush failed; the call stays queued");
        }
    }
}
