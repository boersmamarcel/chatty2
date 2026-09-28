//! Buffered usage event collector with periodic flushing and offline resilience.
//!
//! The [`UsageCollector`] accumulates [`UsageEvent`]s in memory and periodically
//! sends them in batches to the Hive registry.  If the network is unavailable,
//! events are persisted to a local file and retried on the next flush.
//!
//! One collector serves the whole process ([`UsageCollector::global`]): every
//! caller that reuses it also reuses its background flush task, so settings
//! churn (desktop's `refresh_runtime`, for instance) never leaks a second
//! task racing the first over the same queue file (PL-H9, AGE-612).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use chrono::Utc;
use reqwest::StatusCode;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::models::{UsageEvent, UsageReportRequest, UsageReportResponse};
use crate::session::{HiveSession, send_authed};

/// The registry's hard cap on events per `POST /api/usage/report` (answers
/// 422 over this). Not user-configurable: it is the wire protocol, not a
/// buffering knob.
const REGISTRY_BATCH_LIMIT: usize = 100;

/// Backoff floor and ceiling for the background flush loop after a 429/5xx
/// or network error. Any other outcome (success, or a batch dropped as bad)
/// resets the delay back to the floor.
const BACKOFF_FLOOR: Duration = Duration::from_secs(1);
const BACKOFF_CEILING: Duration = Duration::from_secs(15 * 60);

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
    /// Directory for the offline event queue file. Defaults to
    /// `<platform data dir>/chatty` (e.g. `~/.local/share/chatty` on Linux),
    /// never the process's working directory.
    pub queue_dir: PathBuf,
    /// Default reporting policy for free modules.
    pub default_policy: ReportingPolicy,
}

impl Default for UsageCollectorConfig {
    fn default() -> Self {
        Self {
            flush_interval: Duration::from_secs(60),
            max_buffer_size: 100,
            queue_dir: default_queue_dir(),
            default_policy: ReportingPolicy::OptOut { enabled: true },
        }
    }
}

/// The platform data directory's `chatty` subdirectory, falling back to
/// `.chatty` (relative) if the platform directory cannot be determined —
/// same fallback convention as `default_module_dir` in chatty-core.
fn default_queue_dir() -> PathBuf {
    dirs::data_dir()
        .map(|d| d.join("chatty"))
        .unwrap_or_else(|| PathBuf::from(".chatty"))
}

struct CollectorInner {
    buffer: Vec<UsageEvent>,
    base_url: String,
    session: Option<Arc<HiveSession>>,
    config: UsageCollectorConfig,
}

/// Buffered usage event collector.
///
/// Thread-safe via internal `Mutex`.  Call [`UsageCollector::record`] to add
/// events and [`UsageCollector::flush`] (or rely on the background task) to
/// send them to the registry.
pub struct UsageCollector {
    inner: Arc<Mutex<CollectorInner>>,
    http: reqwest::Client,
    /// Serializes offline-queue file writes so a manual `flush()` racing the
    /// background task's own flush can't interleave two partial writes.
    queue_io: Mutex<()>,
    /// The background flush task, if started. A `std::sync::Mutex` because
    /// starting/stopping it never awaits.
    flush_task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Consecutive 429/5xx/network failures, for the background loop's
    /// exponential backoff. Any other outcome resets it to 0.
    consecutive_failures: AtomicU32,
}

/// What happened to a flush, beyond "did it succeed": whether the *cause* of
/// a non-success is worth backing off for (429/5xx/network) or not (a batch
/// the registry told us plainly is bad, or an expired session).
enum FlushOutcome {
    Ok(UsageReportResponse),
    /// A 4xx (other than 401/429) dropped one or more batches outright; the
    /// rest of the queue may still have gone out.
    BatchesDropped(String),
    /// 401 after `send_authed`'s own refresh attempt: the user is signed
    /// out. Everything stays queued.
    Unauthorized,
    /// 429, 5xx, or a network error: everything from here on stays queued
    /// and the background loop should back off before retrying.
    Backoff(String),
}

impl std::fmt::Display for FlushOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlushOutcome::Ok(r) => {
                write!(f, "ok: accepted={} duplicates={}", r.accepted, r.duplicates)
            }
            FlushOutcome::BatchesDropped(e) => write!(f, "batch(es) dropped: {e}"),
            FlushOutcome::Unauthorized => write!(f, "unauthorized"),
            FlushOutcome::Backoff(e) => write!(f, "{e}"),
        }
    }
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
                session: None,
                config,
            })),
            http,
            queue_io: Mutex::new(()),
            flush_task: std::sync::Mutex::new(None),
            consecutive_failures: AtomicU32::new(0),
        }
    }

    /// The one collector for this process (PL-H9, AGE-612): the first
    /// caller's `base_url`/`config` wins and every later caller reuses the
    /// same instance (and, via [`start_background_flush`](Self::start_background_flush),
    /// the same flush task) — desktop's `refresh_runtime` calls this on
    /// every settings change instead of constructing a fresh collector.
    pub fn global(
        base_url: impl Into<String>,
        config: UsageCollectorConfig,
    ) -> Arc<UsageCollector> {
        static GLOBAL: OnceLock<Arc<UsageCollector>> = OnceLock::new();
        Arc::clone(GLOBAL.get_or_init(|| Arc::new(UsageCollector::new(base_url, config))))
    }

    /// Report as the user signed in to `session`.
    pub async fn set_session(&self, session: Arc<HiveSession>) {
        self.inner.lock().await.session = Some(session);
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
    /// Events go out in batches of at most [`REGISTRY_BATCH_LIMIT`]. A batch
    /// the registry rejects outright (a 4xx other than 401/429) is dropped —
    /// logged with its size — and the rest of the queue is still sent; it
    /// does not poison every later flush. A 401 (after `send_authed`'s own
    /// refresh attempt), a 429, a 5xx, or a network error re-queues that
    /// batch and everything after it, unsent, for the next flush.
    pub async fn flush(&self) -> Result<UsageReportResponse, String> {
        match self.flush_inner().await {
            FlushOutcome::Ok(r) => Ok(r),
            other => Err(other.to_string()),
        }
    }

    async fn flush_inner(&self) -> FlushOutcome {
        let (new_events, base_url, session) = {
            let mut inner = self.inner.lock().await;
            let events = std::mem::take(&mut inner.buffer);
            (events, inner.base_url.clone(), inner.session.clone())
        };

        // Serialize against another concurrent flush (background task vs. a
        // manual call) for the whole read-queue/send/write-queue sequence,
        // so the two can't clobber each other's offline queue write.
        let _io = self.queue_io.lock().await;

        let mut remaining = self.load_offline_queue_locked().await;
        remaining.extend(new_events);

        if remaining.is_empty() {
            self.consecutive_failures.store(0, Ordering::Relaxed);
            return FlushOutcome::Ok(UsageReportResponse {
                accepted: 0,
                duplicates: 0,
            });
        }

        let url = format!("{base_url}/api/usage/report");
        let mut total = UsageReportResponse {
            accepted: 0,
            duplicates: 0,
        };
        let mut dropped_error: Option<String> = None;
        let mut stop_error: Option<FlushOutcome> = None;

        while !remaining.is_empty() {
            let take = REGISTRY_BATCH_LIMIT.min(remaining.len());
            let batch: Vec<UsageEvent> = remaining.drain(..take).collect();
            let body = UsageReportRequest {
                events: batch.clone(),
            };

            match send_authed(session.as_deref(), || self.http.post(&url).json(&body)).await {
                Ok(resp) if resp.status().is_success() => match resp.json().await {
                    Ok(r) => {
                        let r: UsageReportResponse = r;
                        total.accepted += r.accepted;
                        total.duplicates += r.duplicates;
                    }
                    Err(e) => {
                        // Can't tell what the registry actually accepted:
                        // keep this batch queued and stop here rather than
                        // risk silently losing it.
                        remaining.splice(0..0, batch);
                        stop_error = Some(FlushOutcome::Backoff(format!("parse error: {e}")));
                        break;
                    }
                },
                Ok(resp) if resp.status() == StatusCode::UNAUTHORIZED => {
                    tracing::warn!("usage report unauthorized — keeping events queued");
                    remaining.splice(0..0, batch);
                    stop_error = Some(FlushOutcome::Unauthorized);
                    break;
                }
                Ok(resp)
                    if resp.status() == StatusCode::TOO_MANY_REQUESTS
                        || resp.status().is_server_error() =>
                {
                    let status = resp.status();
                    tracing::warn!(%status, "usage report failed — queuing for retry");
                    remaining.splice(0..0, batch);
                    stop_error = Some(FlushOutcome::Backoff(format!("server error: {status}")));
                    break;
                }
                Ok(resp) => {
                    // Any other 4xx: the registry is telling us plainly that
                    // this exact batch is bad (F13) — e.g. a bogus
                    // `event_type`, or (belt-and-braces) an over-sized
                    // batch. Re-sending it forever would poison the queue,
                    // so it is dropped, not requeued; the rest of the queue
                    // still goes out below.
                    let status = resp.status();
                    let size = batch.len();
                    tracing::warn!(%status, size, "usage report batch rejected — dropping it");
                    dropped_error
                        .get_or_insert_with(|| format!("batch of {size} rejected: {status}"));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "usage report network error — queuing for retry");
                    remaining.splice(0..0, batch);
                    stop_error = Some(FlushOutcome::Backoff(format!("network error: {e}")));
                    break;
                }
            }
        }

        if remaining.is_empty() {
            self.clear_offline_queue_locked().await;
        } else {
            self.save_offline_queue_locked(&remaining).await;
        }

        match stop_error {
            Some(outcome) => {
                self.note_backoff(&outcome);
                outcome
            }
            None => {
                self.consecutive_failures.store(0, Ordering::Relaxed);
                match dropped_error {
                    Some(e) => FlushOutcome::BatchesDropped(e),
                    None => FlushOutcome::Ok(total),
                }
            }
        }
    }

    fn note_backoff(&self, outcome: &FlushOutcome) {
        match outcome {
            FlushOutcome::Backoff(_) => {
                self.consecutive_failures.fetch_add(1, Ordering::Relaxed);
            }
            _ => self.consecutive_failures.store(0, Ordering::Relaxed),
        }
    }

    /// The delay before the background loop's next flush attempt: the
    /// configured interval, doubled per consecutive 429/5xx/network failure
    /// up to [`BACKOFF_CEILING`].
    async fn next_delay(&self) -> Duration {
        let base = self
            .inner
            .lock()
            .await
            .config
            .flush_interval
            .max(BACKOFF_FLOOR);
        let failures = self.consecutive_failures.load(Ordering::Relaxed);
        if failures == 0 {
            return base;
        }
        base.saturating_mul(1u32 << failures.min(20))
            .min(BACKOFF_CEILING)
    }

    /// Start the background task that periodically flushes the buffer.
    ///
    /// Idempotent: a collector runs at most one flush task at a time. A
    /// second (or hundredth) call — e.g. desktop's `refresh_runtime` firing
    /// again on the next settings change — reuses the task already running
    /// instead of starting another one racing it over the same queue file
    /// (PL-H9, AGE-612). Returns `true` if this call actually started the
    /// task, `false` if one was already running.
    ///
    /// The task holds only a [`Weak`] reference to the collector, so it can
    /// never keep it alive; it exits within one delay of the last strong
    /// reference dropping. Dropping the collector also aborts the task
    /// outright, so it stops immediately rather than waiting out the delay.
    pub fn start_background_flush(self: &Arc<Self>) -> bool {
        let mut guard = self.flush_task.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() {
            return false;
        }

        let weak: Weak<UsageCollector> = Arc::downgrade(self);
        let handle = tokio::spawn(async move {
            loop {
                let Some(collector) = weak.upgrade() else {
                    break;
                };
                let delay = collector.next_delay().await;
                drop(collector);

                tokio::time::sleep(delay).await;

                let Some(collector) = weak.upgrade() else {
                    break;
                };
                if let FlushOutcome::Backoff(e) | FlushOutcome::BatchesDropped(e) =
                    collector.flush_inner().await
                {
                    tracing::debug!(error = %e, "background usage flush did not fully drain the queue");
                }
            }
        });
        *guard = Some(handle);
        true
    }

    // ── Offline queue persistence ──────────────────────────────────────────
    //
    // Every read/write below is only ever called with `queue_io` already
    // held by the caller (`flush_inner`), so the file is never read or
    // written concurrently with itself.

    async fn queue_dir_path(&self) -> PathBuf {
        let inner = self.inner.lock().await;
        inner.config.queue_dir.join("hive-usage-queue.json")
    }

    async fn load_offline_queue_locked(&self) -> Vec<UsageEvent> {
        let path = self.queue_dir_path().await;
        match tokio::fs::read_to_string(&path).await {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    /// Write the queue atomically: a temp file next to the target, then a
    /// rename — never a partial file if the process dies mid-write.
    async fn save_offline_queue_locked(&self, events: &[UsageEvent]) {
        let path = self.queue_dir_path().await;
        let Ok(data) = serde_json::to_string(events) else {
            tracing::warn!("failed to serialize offline usage queue");
            return;
        };
        if let Some(parent) = path.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            tracing::warn!(error = %e, "failed to create offline usage queue dir");
            return;
        }
        let tmp_path = path.with_extension("json.tmp");
        if let Err(e) = tokio::fs::write(&tmp_path, &data).await {
            tracing::warn!(error = %e, "failed to write offline usage queue temp file");
            return;
        }
        if let Err(e) = tokio::fs::rename(&tmp_path, &path).await {
            tracing::warn!(error = %e, "failed to persist offline usage queue");
        }
    }

    async fn clear_offline_queue_locked(&self) {
        let path = self.queue_dir_path().await;
        let _ = tokio::fs::remove_file(&path).await;
    }
}

impl Drop for UsageCollector {
    fn drop(&mut self) {
        if let Some(handle) = self
            .flush_task
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PL-H9 (AGE-612): desktop's `refresh_runtime` calls
    /// `start_background_flush` on every settings change (module toggle,
    /// registry URL edit, ...); two calls back to back — the exact shape of
    /// a rapid edit — must not leak a second flush task racing the first
    /// over the same queue file. `start_background_flush` reports whether it
    /// actually started one.
    #[tokio::test]
    async fn start_background_flush_is_idempotent() {
        let collector = Arc::new(UsageCollector::new(
            "http://usage-collector.invalid",
            UsageCollectorConfig {
                queue_dir: std::env::temp_dir().join(format!("age612-idem-{}", Uuid::new_v4())),
                ..Default::default()
            },
        ));

        assert!(
            collector.start_background_flush(),
            "the first call must start the flush task"
        );
        assert!(
            !collector.start_background_flush(),
            "a second call (as a second `refresh_runtime`) must reuse the running task"
        );
        assert!(
            !collector.start_background_flush(),
            "a third call must still reuse it"
        );
    }

    /// The default queue directory is never the process's working
    /// directory (the pre-AGE-612 default: `.` → `.hive-usage-queue.json`
    /// wherever the process happened to start).
    #[test]
    fn default_queue_dir_is_not_the_working_directory() {
        let dir = default_queue_dir();
        assert_ne!(dir, PathBuf::from("."));
        assert!(
            dir.ends_with("chatty") || dir.ends_with(".chatty"),
            "expected a `chatty` (or `.chatty` fallback) directory, got {}",
            dir.display()
        );
    }
}
