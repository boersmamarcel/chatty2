//! What the MCP handler does around one guest call: find the module, admit
//! it through the meter, run metadata exports off the async workers, record usage, and
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
use hive_client::CallUsage;

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

/// Whether the call to `name` may run: the gateway's [`ModuleMeter`]
/// (AGE-837), taken before any module lock so a slow credit backend holds
/// up nobody else. A paid module needs credits (or a free call left) and a
/// usage collector to report the call (PL-H9, AGE-612); a free one always
/// runs.
///
/// [`ModuleMeter`]: hive_client::ModuleMeter
pub(crate) async fn admit(state: &GatewayState, name: &str) -> Result<(), String> {
    state.meter.admit(name).await.map(|_| ())
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

/// Report one successful invocation through the meter, tagged with the
/// version of the manifest that actually ran (PL-H9, AGE-612) — not a
/// hardcoded `"latest"`, which would misattribute usage to whichever version
/// happens to be newest by the time this reads, not the one that answered
/// the call.
pub(crate) fn record_usage(state: &GatewayState, name: &str, metrics: Option<InvocationMetrics>) {
    let meter = Arc::clone(&state.meter);
    let registry = Arc::clone(&state.registry);
    let name = name.to_string();
    tokio::spawn(async move {
        let version = registry
            .read()
            .await
            .manifest(&name)
            .map(|m| m.version.clone())
            .unwrap_or_else(|| "unknown".to_string());
        meter
            .record(&name, &version, call_usage(metrics.as_ref()))
            .await;
    });
}

/// The runtime's measurements of a call, as the meter reports them.
fn call_usage(metrics: Option<&InvocationMetrics>) -> CallUsage {
    CallUsage {
        input_tokens: metrics.and_then(|m| m.input_tokens.map(|t| t as i32)),
        output_tokens: metrics.and_then(|m| m.output_tokens.map(|t| t as i32)),
        fuel_consumed: metrics.map(|m| m.fuel_consumed),
        execution_ms: metrics.map(|m| m.execution_ms),
    }
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
