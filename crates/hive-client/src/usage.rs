//! Buffered usage event collector with periodic flushing and offline resilience.
//!
//! The [`UsageCollector`] accumulates [`UsageEvent`]s in memory and periodically
//! sends them in batches to the Hive registry.  If the network is unavailable,
//! events are persisted to a local file and retried on the next flush.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::models::{UsageEvent, UsageReportResponse};

/// Controls whether usage reporting can be disabled by the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportingPolicy {
    /// Paid modules — reporting is mandatory and cannot be disabled.
    Required,
    /// Free modules — reporting is enabled by default but user can opt out.
    OptOut { enabled: bool },
}

impl ReportingPolicy {
    pub fn should_report(&self) -> bool {
        match self {
            ReportingPolicy::Required => true,
            ReportingPolicy::OptOut { enabled } => *enabled,
        }
    }
}

/// Configuration for the [`UsageCollector`].
#[derive(Debug, Clone)]
pub struct UsageCollectorConfig {
    /// How often to flush buffered events (default: 60 seconds).
    pub flush_interval: Duration,
    /// Maximum events to buffer before triggering an early flush (default: 100).
    pub max_buffer_size: usize,
    /// Directory for the offline event queue file.
    pub queue_dir: PathBuf,
    /// Default reporting policy for free modules.
    pub default_policy: ReportingPolicy,
}

impl Default for UsageCollectorConfig {
    fn default() -> Self {
        Self {
            flush_interval: Duration::from_secs(60),
            max_buffer_size: 100,
            queue_dir: PathBuf::from("."),
            default_policy: ReportingPolicy::OptOut { enabled: true },
        }
    }
}

struct CollectorInner {
    buffer: Vec<UsageEvent>,
    base_url: String,
    token: Option<String>,
    config: UsageCollectorConfig,
}

/// Buffered usage event collector.
///
/// Thread-safe via internal `Mutex`.  Call [`record`] to add events and
/// [`flush`] (or rely on the background task) to send them to the registry.
pub struct UsageCollector {
    inner: Arc<Mutex<CollectorInner>>,
    http: reqwest::Client,
}

impl UsageCollector {
    /// Create a new collector pointing at the given registry base URL.
    pub fn new(base_url: impl Into<String>, config: UsageCollectorConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();

        Self {
            inner: Arc::new(Mutex::new(CollectorInner {
                buffer: Vec::new(),
                base_url: base_url.into().trim_end_matches('/').to_string(),
                token: None,
                config,
            })),
            http,
        }
    }

    /// Set the Bearer token for authenticated requests.
    pub async fn set_token(&self, token: impl Into<String>) {
        self.inner.lock().await.token = Some(token.into());
    }

    /// Record a usage event.  The event is buffered and will be sent on the
    /// next flush (either periodic or when the buffer is full).
    ///
    /// Returns `true` if the buffer is now full and should be flushed.
    pub async fn record(&self, event: UsageEvent) -> bool {
        let mut inner = self.inner.lock().await;
        if !inner.config.default_policy.should_report() {
            return false;
        }

        inner.buffer.push(event);
        inner.buffer.len() >= inner.config.max_buffer_size
    }

    /// Convenience method to record a module invocation with standard metrics.
    pub async fn record_invocation(
        &self,
        module_name: &str,
        module_version: &str,
        input_tokens: Option<i32>,
        output_tokens: Option<i32>,
        fuel_consumed: Option<u64>,
        execution_ms: Option<u32>,
    ) -> bool {
        let event = UsageEvent {
            idempotency_key: Uuid::new_v4().to_string(),
            module_name: module_name.to_string(),
            module_version: module_version.to_string(),
            event_type: "invocation".to_string(),
            input_tokens,
            output_tokens,
            fuel_consumed: fuel_consumed.map(|f| f as i64),
            execution_ms: execution_ms.map(|m| m as i32),
            metadata: None,
            occurred_at: Utc::now(),
        };
        self.record(event).await
    }

    /// Flush all buffered events to the registry.
    ///
    /// On network failure, events are persisted to the offline queue file.
    /// On success, any previously queued offline events are also submitted.
    pub async fn flush(&self) -> Result<UsageReportResponse, String> {
        let (events, base_url, token) = {
            let mut inner = self.inner.lock().await;
            let events = std::mem::take(&mut inner.buffer);
            let base_url = inner.base_url.clone();
            let token = inner.token.clone();
            (events, base_url, token)
        };

        // Load any offline-queued events
        let mut all_events = self.load_offline_queue().await;
        all_events.extend(events);

        if all_events.is_empty() {
            return Ok(UsageReportResponse {
                accepted: 0,
                duplicates: 0,
            });
        }

        let url = format!("{}/api/usage/report", base_url);
        let body = crate::models::UsageReportRequest {
            events: all_events.clone(),
        };

        let mut request = self.http.post(&url).json(&body);
        if let Some(ref tok) = token {
            request = request.header("Authorization", format!("Bearer {tok}"));
        }

        match request.send().await {
            Ok(resp) if resp.status().is_success() => {
                // Clear the offline queue on success
                self.clear_offline_queue().await;
                resp.json::<UsageReportResponse>()
                    .await
                    .map_err(|e| format!("parse error: {e}"))
            }
            Ok(resp) => {
                let status = resp.status();
                // Don't queue on auth errors — these won't resolve by retrying
                if status == reqwest::StatusCode::UNAUTHORIZED {
                    tracing::warn!("usage report unauthorized — dropping events");
                    return Err("unauthorized".to_string());
                }
                // Queue for retry on server errors
                tracing::warn!(%status, "usage report failed — queuing for retry");
                self.save_offline_queue(&all_events).await;
                Err(format!("server error: {status}"))
            }
            Err(e) => {
                tracing::warn!(error = %e, "usage report network error — queuing for retry");
                self.save_offline_queue(&all_events).await;
                Err(format!("network error: {e}"))
            }
        }
    }

    /// Start a background task that periodically flushes the buffer.
    ///
    /// Returns a [`tokio::task::JoinHandle`] for the background task.
    pub fn start_background_flush(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let collector = Arc::clone(self);
        tokio::spawn(async move {
            let interval = {
                let inner = collector.inner.lock().await;
                inner.config.flush_interval
            };
            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await; // skip the immediate first tick
            loop {
                ticker.tick().await;
                if let Err(e) = collector.flush().await {
                    tracing::debug!(error = %e, "background usage flush failed");
                }
            }
        })
    }

    // ── Offline queue persistence ──────────────────────────────────────────

    async fn queue_dir_path(&self) -> PathBuf {
        let inner = self.inner.lock().await;
        inner.config.queue_dir.join(".hive-usage-queue.json")
    }

    async fn load_offline_queue(&self) -> Vec<UsageEvent> {
        let path = self.queue_dir_path().await;
        match tokio::fs::read_to_string(&path).await {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    async fn save_offline_queue(&self, events: &[UsageEvent]) {
        let path = self.queue_dir_path().await;
        if let Ok(data) = serde_json::to_string(events) {
            if let Err(e) = tokio::fs::write(&path, data).await {
                tracing::warn!(error = %e, "failed to write offline usage queue");
            }
        }
    }

    async fn clear_offline_queue(&self) {
        let path = self.queue_dir_path().await;
        let _ = tokio::fs::remove_file(&path).await;
    }
}
