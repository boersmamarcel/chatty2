//! How many workers may hold one model endpoint at a time (ADR-0011 C6).
//!
//! Delegation fans out: a parent can ask for three workers in one turn, and
//! every worker is a full chatty process that talks to a model server. When
//! that server is a local one, three concurrent requests against a single
//! loaded model is not three times the throughput — Ollama serves
//! `OLLAMA_NUM_PARALLEL` requests per model and evicts to make room for the
//! rest, so the fourth worker pays for a load/unload cycle that the first
//! three also pay for again. The fix is not to spawn it yet.
//!
//! So the broker holds one semaphore per *endpoint* — a model server's base
//! URL, not a model and not a worker — and a task waits for a permit before
//! a child is spawned. Waiting costs a queued task nothing but time; the
//! alternative costs every task on that endpoint.
//!
//! # What is metered
//!
//! The permit is held from just before the spawn until the worker is reaped,
//! which is deliberately wider than the model request itself: between those
//! two points the child is a process holding a workspace, and it is that
//! whole occupancy the budget is sizing, not the seconds it spends streaming.
//!
//! # Queue depth
//!
//! Every wait is a `tracing` event carrying the endpoint, its limit and the
//! depth at that moment, and [`EndpointBudget::queue_depth`] reads the same
//! number live. A budget that is too tight looks like a queue that never
//! empties, and that has to be visible without a debugger.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{debug, info};

/// The limit used for an endpoint nothing has been said about.
///
/// One, because the endpoint that needs a budget is a local model server, and
/// a local model server that has not told us otherwise serves one request at
/// a time. Raise it per endpoint, or globally, from settings.
pub const DEFAULT_ENDPOINT_LIMIT: usize = 1;

/// Per-endpoint concurrency limits for the broker's workers.
///
/// Cheap to clone; all clones share one set of semaphores, which is what
/// makes the budget a property of the endpoint rather than of a caller.
#[derive(Clone)]
pub struct EndpointBudget {
    inner: Arc<Inner>,
}

struct Inner {
    default_limit: usize,
    limits: HashMap<String, usize>,
    /// Endpoints that have been used at least once. Created lazily so a
    /// budget can be configured for endpoints this process never touches.
    live: Mutex<HashMap<String, Arc<Endpoint>>>,
}

struct Endpoint {
    semaphore: Arc<Semaphore>,
    limit: usize,
    /// Tasks waiting for a permit right now — see [`EndpointBudget::queue_depth`].
    queued: AtomicUsize,
}

impl EndpointBudget {
    /// A budget where every endpoint gets `default_limit`, clamped to at
    /// least one: a limit of zero is not a budget, it is a deadlock.
    pub fn new(default_limit: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                default_limit: default_limit.max(1),
                limits: HashMap::new(),
                live: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Override the limit for one endpoint.
    ///
    /// A builder, and only a builder: the limits are fixed before the budget
    /// is shared, so this panics rather than silently dropping an override if
    /// it is called on a clone.
    pub fn with_endpoint(mut self, endpoint: impl Into<String>, limit: usize) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("an endpoint budget is configured before it is shared")
            .limits
            .insert(endpoint.into(), limit.max(1));
        self
    }

    /// The limit in force for `endpoint`.
    pub fn limit(&self, endpoint: &str) -> usize {
        self.inner
            .limits
            .get(endpoint)
            .copied()
            .unwrap_or(self.inner.default_limit)
    }

    /// Wait for a slot on `endpoint`.
    ///
    /// The returned permit releases the slot when it is dropped, on every
    /// path — the task finishing, the caller hanging up, the broker shutting
    /// down — because a slot that is only released on the happy path is a
    /// budget that shrinks to nothing over a session.
    pub async fn acquire(&self, endpoint: &str) -> EndpointPermit {
        let live = self.endpoint(endpoint);

        // The common case is an idle endpoint, and it should not pay for the
        // logging the queued case wants.
        if let Ok(permit) = live.semaphore.clone().try_acquire_owned() {
            return EndpointPermit { _permit: permit };
        }

        let queued = QueuedGuard::enter(&live);
        info!(
            endpoint = %endpoint,
            limit = live.limit,
            queue_depth = queued.depth(),
            "Delegated task is waiting for a slot on a busy model endpoint"
        );

        let waiting_since = Instant::now();
        let permit = live
            .semaphore
            .clone()
            .acquire_owned()
            .await
            // The semaphore is owned by this budget and never closed; the
            // only error `acquire_owned` has is closure.
            .expect("an endpoint's semaphore is never closed");
        let waited = waiting_since.elapsed();
        drop(queued);

        debug!(
            endpoint = %endpoint,
            limit = live.limit,
            waited_ms = waited.as_millis() as u64,
            queue_depth = live.queued.load(Ordering::Relaxed),
            "Delegated task admitted to a model endpoint"
        );
        EndpointPermit { _permit: permit }
    }

    /// How many tasks are waiting for a slot on `endpoint` right now.
    ///
    /// Zero for an endpoint that is busy but has nobody queued behind it —
    /// this is the backlog, not the occupancy. See [`Self::in_flight`].
    pub fn queue_depth(&self, endpoint: &str) -> usize {
        self.live(endpoint)
            .map(|e| e.queued.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// How many slots on `endpoint` are held right now.
    pub fn in_flight(&self, endpoint: &str) -> usize {
        self.live(endpoint)
            .map(|e| e.limit.saturating_sub(e.semaphore.available_permits()))
            .unwrap_or(0)
    }

    fn live(&self, endpoint: &str) -> Option<Arc<Endpoint>> {
        self.lock().get(endpoint).cloned()
    }

    /// The endpoint's semaphore, made on first use at the limit in force.
    fn endpoint(&self, endpoint: &str) -> Arc<Endpoint> {
        let limit = self.limit(endpoint);
        let mut live = self.lock();
        Arc::clone(live.entry(endpoint.to_string()).or_insert_with(|| {
            debug!(endpoint = %endpoint, limit, "Metering a model endpoint");
            Arc::new(Endpoint {
                semaphore: Arc::new(Semaphore::new(limit)),
                limit,
                queued: AtomicUsize::new(0),
            })
        }))
    }

    /// A poisoned lock means a panic happened while an entry was being
    /// added. The map holds `Arc`s with no cross-entry invariant, so
    /// recovering it is sound and better than taking every delegation on the
    /// broker down with it.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Endpoint>>> {
        self.inner.live.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Default for EndpointBudget {
    fn default() -> Self {
        Self::new(DEFAULT_ENDPOINT_LIMIT)
    }
}

impl std::fmt::Debug for EndpointBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EndpointBudget")
            .field("default_limit", &self.inner.default_limit)
            .field("limits", &self.inner.limits)
            .finish_non_exhaustive()
    }
}

/// One slot on an endpoint, released when this is dropped.
#[derive(Debug)]
pub struct EndpointPermit {
    _permit: OwnedSemaphorePermit,
}

/// Counts one waiter for as long as it is alive.
///
/// A guard rather than a pair of `fetch_add`/`fetch_sub` calls because the
/// wait is cancellable: an A2A caller that hangs up drops the future between
/// the two, and a queue depth that only ever goes up is worse than no queue
/// depth at all.
struct QueuedGuard {
    endpoint: Arc<Endpoint>,
    depth: usize,
}

impl QueuedGuard {
    fn enter(endpoint: &Arc<Endpoint>) -> Self {
        let depth = endpoint.queued.fetch_add(1, Ordering::Relaxed) + 1;
        Self {
            endpoint: Arc::clone(endpoint),
            depth,
        }
    }

    /// The depth including this waiter, as it was on entry.
    fn depth(&self) -> usize {
        self.depth
    }
}

impl Drop for QueuedGuard {
    fn drop(&mut self) {
        self.endpoint.queued.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn a_budget_of_two_admits_two_and_queues_the_third() {
        let budget = EndpointBudget::new(2);
        let endpoint = "http://localhost:11434";

        let first = budget.acquire(endpoint).await;
        let second = budget.acquire(endpoint).await;
        assert_eq!(budget.in_flight(endpoint), 2);
        assert_eq!(budget.queue_depth(endpoint), 0, "nobody is waiting yet");

        let waiting = tokio::spawn({
            let budget = budget.clone();
            async move { budget.acquire(endpoint).await }
        });
        // Let the third reach the semaphore.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished(), "the third task waits its turn");
        assert_eq!(budget.queue_depth(endpoint), 1);

        drop(first);
        let third = tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .expect("the third task is admitted once a slot frees")
            .unwrap();

        assert_eq!(budget.queue_depth(endpoint), 0);
        assert_eq!(budget.in_flight(endpoint), 2);
        drop((second, third));
        assert_eq!(budget.in_flight(endpoint), 0);
    }

    #[tokio::test]
    async fn each_endpoint_queues_on_its_own() {
        let budget = EndpointBudget::new(1);
        let _busy = budget.acquire("http://localhost:11434").await;

        // A different endpoint is a different semaphore, so this returns
        // rather than queueing behind the first.
        let other = tokio::time::timeout(
            Duration::from_millis(200),
            budget.acquire("https://openrouter.ai/api/v1"),
        )
        .await
        .expect("an idle endpoint admits immediately");

        assert_eq!(budget.in_flight("http://localhost:11434"), 1);
        assert_eq!(budget.in_flight("https://openrouter.ai/api/v1"), 1);
        drop(other);
    }

    #[tokio::test]
    async fn a_per_endpoint_override_beats_the_default() {
        let budget = EndpointBudget::new(1).with_endpoint("http://localhost:11434", 3);
        assert_eq!(budget.limit("http://localhost:11434"), 3);
        assert_eq!(budget.limit("https://openrouter.ai/api/v1"), 1);

        let held: Vec<_> = vec![
            budget.acquire("http://localhost:11434").await,
            budget.acquire("http://localhost:11434").await,
            budget.acquire("http://localhost:11434").await,
        ];
        assert_eq!(budget.in_flight("http://localhost:11434"), 3);
        drop(held);
    }

    #[tokio::test]
    async fn a_cancelled_waiter_does_not_leak_queue_depth() {
        let budget = EndpointBudget::new(1);
        let endpoint = "http://localhost:11434";
        let _held = budget.acquire(endpoint).await;

        let waiting = tokio::spawn({
            let budget = budget.clone();
            async move { budget.acquire(endpoint).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(budget.queue_depth(endpoint), 1);

        waiting.abort();
        let _ = waiting.await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            budget.queue_depth(endpoint),
            0,
            "a caller that hung up is not still queued"
        );
    }

    #[test]
    fn a_zero_limit_is_treated_as_one_rather_than_a_deadlock() {
        let budget = EndpointBudget::new(0).with_endpoint("x", 0);
        assert_eq!(budget.limit("x"), 1);
        assert_eq!(budget.limit("anything-else"), 1);
    }
}
