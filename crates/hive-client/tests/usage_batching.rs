//! `UsageCollector::flush` batching and error handling (PL-H9, AGE-612 /
//! evaluation plan row 5.8): a batch the registry rejects outright is
//! dropped, not re-sent forever, and more than the registry's 100-event
//! limit goes out as several batches within one flush. These mirror
//! `hive-e2e`'s ignored `s5_08_*` tests (which need a live stack) with a
//! wiremock double instead, so they run on every PR, not just the nightly.

use std::path::Path;

use chrono::Utc;
use hive_client::models::UsageEvent;
use hive_client::{UsageCollector, UsageCollectorConfig};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

fn event(idempotency_key: &str) -> UsageEvent {
    UsageEvent {
        idempotency_key: idempotency_key.to_string(),
        module_name: "echo-agent".to_string(),
        module_version: "1.0.0".to_string(),
        event_type: "invocation".to_string(),
        input_tokens: Some(3),
        output_tokens: Some(2),
        fuel_consumed: None,
        execution_ms: Some(1),
        metadata: None,
        occurred_at: Utc::now(),
    }
}

/// Matches a `POST /api/usage/report` body whose `events` carry exactly
/// these idempotency keys, in this order — precise enough that two mocks
/// for same-sized batches (e.g. a rejected one-event batch and the next,
/// different, one-event batch) can never be mixed up by mock-selection
/// order.
struct Batch(Vec<String>);

fn keys(ids: impl IntoIterator<Item = impl Into<String>>) -> Batch {
    Batch(ids.into_iter().map(Into::into).collect())
}

impl Match for Batch {
    fn matches(&self, request: &Request) -> bool {
        let Ok(body) = request.body_json::<serde_json::Value>() else {
            return false;
        };
        let Some(events) = body.get("events").and_then(|e| e.as_array()) else {
            return false;
        };
        let got: Vec<&str> = events
            .iter()
            .filter_map(|e| e.get("idempotency_key")?.as_str())
            .collect();
        got.len() == self.0.len() && got.iter().zip(&self.0).all(|(a, b)| *a == b)
    }
}

/// The number of events still in the offline queue directory.
fn queued(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .expect("the queue dir")
        .filter_map(Result::ok)
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .filter_map(|text| serde_json::from_str::<Vec<serde_json::Value>>(&text).ok())
        .map(|events| events.len())
        .sum()
}

fn collector(server: &MockServer, queue_dir: &Path) -> UsageCollector {
    UsageCollector::new(
        server.uri(),
        UsageCollectorConfig {
            queue_dir: queue_dir.to_path_buf(),
            max_buffer_size: 1000,
            ..Default::default()
        },
    )
}

/// A batch the registry answers 400 (a bogus event) is dropped outright —
/// not re-queued — and a later flush's own (valid) batch still goes out.
/// Before PL-H9 the rejected batch was re-queued and re-sent forever (F13).
#[tokio::test]
async fn a_400_batch_is_dropped_and_the_next_batch_still_sends() {
    let server = MockServer::start().await;
    let queue = tempfile::tempdir().expect("a queue dir");
    let collector = collector(&server, queue.path());

    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/api/usage/report"))
        .and(keys(["bad"]))
        .respond_with(ResponseTemplate::new(400))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    collector.record(event("bad")).await;
    let rejected = collector.flush().await;
    assert!(
        rejected.is_err(),
        "a 400 response must surface as an error: {rejected:?}"
    );
    assert_eq!(
        queued(queue.path()),
        0,
        "the rejected batch must be dropped, not re-queued"
    );

    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/api/usage/report"))
        .and(keys(["good"]))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "accepted": 1, "duplicates": 0 })),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    collector.record(event("good")).await;
    let next = collector.flush().await;
    assert!(
        matches!(next, Ok(ref r) if r.accepted == 1),
        "the next (valid) batch must still be sent: {next:?}"
    );
    assert_eq!(queued(queue.path()), 0, "the queue must be empty after");
}

/// A 422 (the registry's actual status for a batch it rejects, e.g. over its
/// 100-event limit) is treated the same as any other non-401/429 4xx: the
/// offending batch is dropped, not re-queued.
#[tokio::test]
async fn a_422_batch_is_dropped_not_requeued() {
    let server = MockServer::start().await;
    let queue = tempfile::tempdir().expect("a queue dir");
    let collector = collector(&server, queue.path());

    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/api/usage/report"))
        .and(keys(["unprocessable"]))
        .respond_with(ResponseTemplate::new(422))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    collector.record(event("unprocessable")).await;
    let rejected = collector.flush().await;
    assert!(rejected.is_err(), "a 422 must surface as an error too");
    assert_eq!(queued(queue.path()), 0, "a 422 batch is dropped, not kept");
}

/// 150 events go out in two batches — 100, then 50 — within one `flush()`
/// call, and the queue drains completely. Before PL-H9 all 150 went in one
/// request over the registry's 100-event limit, the registry rejected it,
/// and the whole lot was re-queued and re-sent forever (F13).
#[tokio::test]
async fn a_150_event_flush_sends_100_then_50() {
    let server = MockServer::start().await;
    let queue = tempfile::tempdir().expect("a queue dir");
    let collector = collector(&server, queue.path());

    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/api/usage/report"))
        .and(keys((0..100).map(|i| format!("e{i}"))))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "accepted": 100, "duplicates": 0 })),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/api/usage/report"))
        .and(keys((100..150).map(|i| format!("e{i}"))))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "accepted": 50, "duplicates": 0 })),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    for i in 0..150 {
        collector.record(event(&format!("e{i}"))).await;
    }

    let flushed = collector.flush().await;
    assert!(
        matches!(flushed, Ok(ref r) if r.accepted == 150),
        "150 events must all be accepted across the two batches: {flushed:?}"
    );
    assert_eq!(
        queued(queue.path()),
        0,
        "the queue must be fully drained after the batches went out"
    );
}
