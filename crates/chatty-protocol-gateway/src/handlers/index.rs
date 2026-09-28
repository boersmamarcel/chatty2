//! Index handler: `GET /` — JSON listing of all modules and their endpoints.
//! A plugin is served over MCP only (PL-U3).

use axum::{Json, extract::State, response::IntoResponse};
use serde_json::{Value, json};

use crate::gateway::GatewayState;

// ---------------------------------------------------------------------------
// Handler: GET /
// ---------------------------------------------------------------------------

pub(crate) async fn index(State(state): State<GatewayState>) -> impl IntoResponse {
    let reg = state.registry.read().await;

    let modules: Vec<Value> = reg
        .module_names()
        .map(|name| {
            let protocols = reg
                .manifest(name)
                .map(|m| {
                    json!({
                        "mcp": m.protocols.mcp,
                    })
                })
                .unwrap_or(json!({}));

            let mut endpoints = Vec::<Value>::new();

            if reg.manifest(name).is_some_and(|m| m.protocols.mcp) {
                endpoints.push(json!({
                    "method": "POST",
                    "path": format!("/mcp/{}", name),
                    "description": "MCP JSON-RPC (tools/list, tools/call)"
                }));
                endpoints.push(json!({
                    "method": "GET",
                    "path": format!("/mcp/{}/sse", name),
                    "description": "MCP SSE transport"
                }));
            }

            json!({
                "name": name,
                "protocols": protocols,
                "endpoints": endpoints,
            })
        })
        .collect();

    Json(json!({
        "gateway": "chatty-protocol-gateway",
        "modules": modules,
        "global_endpoints": [
            { "method": "GET", "path": "/.well-known/agent.json", "description": "Aggregated A2A agent card (participants and virtual agents)" },
        ]
    }))
}
