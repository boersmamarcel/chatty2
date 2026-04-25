//! Index handler: `GET /` — JSON listing of all modules and their endpoints.

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
                        "openai_compat": m.protocols.openai_compat,
                        "mcp": m.protocols.mcp,
                        "a2a": m.protocols.a2a,
                    })
                })
                .unwrap_or(json!({}));

            let mut endpoints = Vec::<Value>::new();

            if let Some(manifest) = reg.manifest(name) {
                if manifest.protocols.openai_compat {
                    endpoints.push(json!({
                        "method": "POST",
                        "path": format!("/v1/{}/chat/completions", name),
                        "description": "OpenAI-compatible chat completion"
                    }));
                }
                if manifest.protocols.mcp {
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
                if manifest.protocols.a2a {
                    endpoints.push(json!({
                        "method": "GET",
                        "path": format!("/a2a/{}/.well-known/agent.json", name),
                        "description": "A2A agent card"
                    }));
                    endpoints.push(json!({
                        "method": "POST",
                        "path": format!("/a2a/{}", name),
                        "description": "A2A JSON-RPC (message/send, tasks/get)"
                    }));
                }
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
            { "method": "GET", "path": "/.well-known/agent.json", "description": "Aggregated A2A agent card" },
            { "method": "POST", "path": "/v1/chat/completions", "description": "OpenAI-compatible chat completion (model-routed via module:{name})" },
        ]
    }))
}
