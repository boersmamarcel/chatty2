//! AGE-1293: a worker's `search_web` uses the search provider the settings
//! name, not the keyless engines.
//!
//! The kit writes a `search_settings.json` for its workers the way the
//! user's own sits in their config directory; its `managed` block points
//! Tavily's shape at a fake server on loopback, so nothing leaves the
//! machine. A worker that ignored the file would search keyless and the
//! fake server would see no request.

use std::sync::{Arc, Mutex};

use chatty_core::settings::models::search_settings::{
    ManagedSearch, SearchProvider, SearchSettingsModel,
};
use chatty_core::testing::fake_model::{Reply, Script};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::swarm_kit::{AgentDef, Endpoint, SwarmKit};

const SEARCHER: &str = "searcher";
const SEARCHER_MODEL: &str = "searcher-model";
const KEY: &str = "tvly-test";

/// A Tavily stand-in on loopback: answers every request with one result and
/// keeps the requests it saw. Returns its base URL.
async fn fake_tavily() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16384];
                let Ok(n) = socket.read(&mut buf).await else {
                    return;
                };
                log.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf[..n]).to_lowercase());
                let body = r#"{"results":[{"title":"From Tavily","url":"https://example.com/t","content":"c"}]}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    (format!("http://{addr}"), seen)
}

/// A worker's `search_web` reaches the configured Tavily endpoint with the
/// configured token, and says so in its result.
#[tokio::test]
async fn a_workers_search_web_uses_the_configured_provider() {
    let (endpoint, seen) = fake_tavily().await;
    let search = SearchSettingsModel {
        enabled: true,
        active_provider: SearchProvider::Tavily,
        managed: Some(ManagedSearch {
            provider: SearchProvider::Tavily,
            endpoint,
            token: KEY.to_string(),
        }),
        ..SearchSettingsModel::default()
    };
    let ndjson = Script::new().route(
        SEARCHER_MODEL,
        [
            Reply::tool_call("search_web", json!({ "query": "agent memory" })),
            Reply::text("Done."),
        ],
    );
    let sse = Script::new().route("unused", [Reply::text("unused")]);
    let kit = SwarmKit::start_with_search(
        search,
        vec![AgentDef::new(SEARCHER, SEARCHER_MODEL, Endpoint::Ndjson).profile("web-researcher")],
        sse,
        ndjson,
    )
    .await;

    let run = kit.run_leader("Search for agent memory.").await;
    run.output.expect("the delegation succeeds");

    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 1, "the worker reached the fake Tavily once");
    assert!(requests[0].starts_with("post /search "), "{}", requests[0]);
    assert!(
        requests[0].contains(&format!("authorization: bearer {KEY}")),
        "{}",
        requests[0]
    );

    // The model's second request carries the tool's result, from Tavily.
    let model_requests = kit.ndjson.requests_for(SEARCHER_MODEL);
    let second = model_requests
        .get(1)
        .expect("the worker went back to its model");
    let body = String::from_utf8_lossy(&second.body).to_string();
    assert!(body.contains("From Tavily"), "{body}");
}
