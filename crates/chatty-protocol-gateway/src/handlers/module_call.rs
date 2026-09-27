//! What every protocol handler does around one guest call: find the module
//! for a protocol, check credits, run metadata exports off the async
//! workers, record usage, and pick the status for a failed call.
//!
//! The registry lock is held only for the lookup. The call itself runs
//! under the module's own [`ModuleHandle`] lock, so modules never wait on
//! each other (PL-H4, AGE-607).

use std::sync::Arc;

use axum::http::StatusCode;
use chatty_module_registry::ModuleHandle;
use chatty_wasm_runtime::{CallError, InvocationMetrics, WasmModule};

use crate::gateway::GatewayState;

/// The three protocol surfaces a module's `[protocols]` table switches.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Protocol {
    OpenAi,
    Mcp,
    A2a,
}

/// The handle of `name`, if it is loaded **and** its manifest enables
/// `protocol`. A disabled protocol answers exactly like a missing module
/// (404): the module is not served there.
pub(crate) async fn module_for(
    state: &GatewayState,
    name: &str,
    protocol: Protocol,
) -> Option<ModuleHandle> {
    let reg = state.registry.read().await;
    let protocols = &reg.manifest(name)?.protocols;
    let enabled = match protocol {
        Protocol::OpenAi => protocols.openai_compat,
        Protocol::Mcp => protocols.mcp,
        Protocol::A2a => protocols.a2a,
    };
    if enabled { reg.get(name) } else { None }
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

/// Run a synchronous export (`list_tools`, `agent_card`) under the module's
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

/// Report one successful invocation to the usage collector, if any.
pub(crate) fn record_usage(state: &GatewayState, name: &str, metrics: Option<InvocationMetrics>) {
    let Some(usage) = state.usage.as_ref() else {
        return;
    };
    let usage = Arc::clone(usage);
    let name = name.to_string();
    tokio::spawn(async move {
        let metrics = metrics.as_ref();
        usage
            .record_invocation(
                &name,
                "latest",
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
