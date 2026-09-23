//! Record/replay cache for the raw HTTP responses behind `search_web` (and,
//! later, fetched pages), used by the AGE-515/516 retrieval eval
//! (`examples/search_eval.rs`). Not used in the product: a tool without a
//! cache talks to the network exactly as before.
//!
//! Entries hold the *raw* response (status + body), keyed by backend and
//! request, so a replay re-runs every parser and pipeline stage on recorded
//! bytes: changing ranking or extraction costs no API credits and no
//! scraping.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::tools::ToolError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMode {
    /// Serve recorded responses; fetch and record anything missing.
    Record,
    /// Serve recorded responses only; a miss is an error (`replay miss`).
    Replay,
}

/// One recorded exchange. A transport failure (timeout, DNS, TLS) is
/// recorded too, so a replay reproduces the run's errors, not just its hits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedResponse {
    pub status: u16,
    pub body: String,
    #[serde(default)]
    pub transport_error: Option<String>,
    pub latency_ms: u64,
}

#[derive(Debug)]
pub struct ResponseCache {
    dir: PathBuf,
    mode: CacheMode,
    /// On a cache hit, sleep for the recorded latency, so replayed runs keep
    /// realistic (and parallel-composed) latency numbers.
    simulate_latency: bool,
}

impl ResponseCache {
    pub fn new(dir: impl Into<PathBuf>, mode: CacheMode, simulate_latency: bool) -> Self {
        Self {
            dir: dir.into(),
            mode,
            simulate_latency,
        }
    }

    fn path(&self, source: &str, key: &str) -> PathBuf {
        let digest = Sha256::digest(key.as_bytes());
        let name: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
        self.dir.join(source).join(format!("{name}.json"))
    }

    /// Return the recorded response for `(source, key)`, or run `fetch` and
    /// record its result (in [`CacheMode::Record`]).
    pub async fn get_or_fetch<F, Fut>(
        &self,
        source: &str,
        key: &str,
        fetch: F,
    ) -> Result<CachedResponse, ToolError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = CachedResponse>,
    {
        let path = self.path(source, key);
        if let Ok(text) = tokio::fs::read_to_string(&path).await
            && let Ok(entry) = serde_json::from_str::<CacheEntry>(&text)
        {
            if self.simulate_latency {
                tokio::time::sleep(Duration::from_millis(entry.response.latency_ms)).await;
            }
            return Ok(entry.response);
        }
        if self.mode == CacheMode::Replay {
            return Err(ToolError::OperationFailed(format!(
                "replay miss: no recorded {source} response for {key:?}"
            )));
        }
        let response = fetch().await;
        let entry = CacheEntry {
            source: source.to_string(),
            key: key.to_string(),
            response,
        };
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        // Write-then-rename so a concurrent reader never sees half a file.
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        if tokio::fs::write(&tmp, serde_json::to_vec(&entry).unwrap_or_default())
            .await
            .is_ok()
        {
            let _ = tokio::fs::rename(&tmp, &path).await;
        }
        Ok(entry.response)
    }
}

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    source: String,
    key: String,
    response: CachedResponse,
}

/// Send `request` and capture status, body and latency, turning transport
/// failures into a recorded [`CachedResponse`] rather than an error.
pub async fn capture(request: reqwest::RequestBuilder) -> CachedResponse {
    let started = Instant::now();
    let result = async {
        let response = request.send().await?;
        let status = response.status().as_u16();
        let body = response.text().await?;
        Ok::<_, reqwest::Error>((status, body))
    }
    .await;
    let latency_ms = started.elapsed().as_millis() as u64;
    match result {
        Ok((status, body)) => CachedResponse {
            status,
            body,
            transport_error: None,
            latency_ms,
        },
        Err(e) => CachedResponse {
            status: 0,
            body: String::new(),
            transport_error: Some(e.to_string()),
            latency_ms,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(body: &str) -> CachedResponse {
        CachedResponse {
            status: 200,
            body: body.to_string(),
            transport_error: None,
            latency_ms: 5,
        }
    }

    #[tokio::test]
    async fn record_then_replay() {
        let dir = tempfile::tempdir().unwrap();
        let rec = ResponseCache::new(dir.path(), CacheMode::Record, false);
        let got = rec
            .get_or_fetch("bing", "q1", || async { resp("first") })
            .await
            .unwrap();
        assert_eq!(got.body, "first");
        // Recorded: a second fetch closure is never run.
        let got = rec
            .get_or_fetch("bing", "q1", || async { resp("second") })
            .await
            .unwrap();
        assert_eq!(got.body, "first");

        let replay = ResponseCache::new(dir.path(), CacheMode::Replay, false);
        assert_eq!(
            replay
                .get_or_fetch("bing", "q1", || async { resp("x") })
                .await
                .unwrap()
                .body,
            "first"
        );
        let miss = replay
            .get_or_fetch("bing", "q2", || async { resp("x") })
            .await;
        assert!(matches!(miss, Err(ToolError::OperationFailed(m)) if m.starts_with("replay miss")));
        // Keys are per source.
        assert!(
            replay
                .get_or_fetch("duckduckgo", "q1", || async { resp("x") })
                .await
                .is_err()
        );
    }
}
