//! What the MCP handler does around one guest call: find the module, check
//! credits, run metadata exports off the async workers, record usage, and
//! pick the status for a failed call. MCP is the one protocol a plugin is
//! served on (PL-U3): it has tools, and no loop to answer OpenAI or A2A.
//!
//! The registry lock is held only for the lookup. The call itself runs
//! under the module's own [`ModuleHandle`] lock, so modules never wait on
//! each other (PL-H4, AGE-607).

use std::sync::Arc;

use axum::http::StatusCode;
use chatty_module_registry::ModuleHandle;
use chatty_wasm_runtime::{CallError, InvocationMetrics, WasmModule};

use crate::gateway::GatewayState;

/// The handle of `name`, if it is loaded **and** its manifest enables MCP
/// (`[protocols] mcp = true`). A module without it answers exactly like a
/// missing module (404): it is not served.
pub(crate) async fn mcp_module(state: &GatewayState, name: &str) -> Option<ModuleHandle> {
    let reg = state.registry.read().await;
    if reg.manifest(name)?.protocols.mcp {
        reg.get(name)
    } else {
        None
    }
}

/// The pre-invocation credit check for a paid module, taken before any
/// module lock so a slow credit backend holds up nobody else.
pub(crate) async fn check_credits(state: &GatewayState, name: &str) -> Result<(), String> {
    match state.credit_guard.as_ref() {
        Some(guard) if state.paid_modules.contains(name) => {
            guard.has_credits(name).await.map_err(|e| e.to_string())
        }
        _ => Ok(()),
    }
}

/// The pre-invocation usage-reporting check for a paid module (PL-H9,
/// AGE-612): a paid module's usage reporting is `ReportingPolicy::Required`,
/// so a call that cannot be queued for reporting must not run unreported —
/// refuse it here rather than run it and silently fail to report it after.
/// Free (unpaid) modules are unaffected: no usage collector just means no
/// analytics, which is the existing opt-out behaviour.
pub(crate) fn check_usage_reporting(state: &GatewayState, name: &str) -> Result<(), String> {
    if state.paid_modules.contains(name) && state.usage.is_none() {
        return Err(format!(
            "usage reporting is required for paid module '{name}' but no usage collector is configured"
        ));
    }
    Ok(())
}

/// Run a synchronous export (`list_tools`, `metadata`) under the module's
/// lock on the blocking pool, so neither the wait for the guest nor the
/// guest itself occupies a Tokio worker.
pub(crate) async fn blocking<T: Send + 'static>(
    handle: ModuleHandle,
    call: impl FnOnce(&mut WasmModule) -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    let mut module = handle.lock_owned().await;
    tokio::task::spawn_blocking(move || call(&mut module))
        .await
        .map_err(|e| anyhow::anyhow!("module call panicked: {e}"))?
}

/// Report one successful invocation to the usage collector, if any, tagged
/// with the version of the manifest that actually ran (PL-H9, AGE-612) —
/// not a hardcoded `"latest"`, which would misattribute usage to whichever
/// version happens to be newest by the time this reads, not the one that
/// answered the call.
pub(crate) fn record_usage(state: &GatewayState, name: &str, metrics: Option<InvocationMetrics>) {
    let Some(usage) = state.usage.as_ref() else {
        return;
    };
    let usage = Arc::clone(usage);
    let registry = Arc::clone(&state.registry);
    let name = name.to_string();
    tokio::spawn(async move {
        let version = registry
            .read()
            .await
            .manifest(&name)
            .map(|m| m.version.clone())
            .unwrap_or_else(|| "unknown".to_string());
        let metrics = metrics.as_ref();
        usage
            .record_invocation(
                &name,
                &version,
                metrics.and_then(|m| m.input_tokens.map(|t| t as i32)),
                metrics.and_then(|m| m.output_tokens.map(|t| t as i32)),
                metrics.map(|m| m.fuel_consumed),
                metrics.map(|m| m.execution_ms),
            )
            .await;
    });
}

/// The HTTP status for a failed guest call. A reply over the output cap
/// (PL-H1) is the module answering badly, so it is a 502 with a short body,
/// never the reply relayed; anything else is the gateway's 500.
pub(crate) fn failure_status(err: &anyhow::Error) -> StatusCode {
    match err.downcast_ref::<CallError>() {
        Some(CallError::OutputTooLarge { .. }) => StatusCode::BAD_GATEWAY,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
