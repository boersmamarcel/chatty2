//! How many runs may talk to one model endpoint at a time (ADR-0011 C6,
//! ADR-0020 §3.5).
//!
//! Delegation fans out: a parent can ask for three workers in one turn, and
//! every worker is a full chatty process that talks to a model server. When
//! that server is a local one, three concurrent requests against a single
//! loaded model is not three times the throughput — Ollama serves
//! `OLLAMA_NUM_PARALLEL` requests per model and evicts to make room for the
//! rest, so the fourth worker pays for a load/unload cycle that the first
//! three also pay for again. The fix is not to let it talk yet.
//!
//! So the broker holds one semaphore per *endpoint* — a model server's base
//! URL, not a model and not a worker — in an [`EndpointBudget`]. The
//! semaphore is `tokio`'s, which is fair: waiters are served in the order
//! they arrived.
//!
//! # What a permit covers: a run's model calls
//!
//! A run takes its endpoint's permit before its worker is spawned, and a
//! [`RunPermit`] then tracks it through the run:
//!
//! ```text
//! Holding ──first child call──▶ Released{outstanding} ──last result──▶ Reacquiring ──▶ Holding
//! ```
//!
//! - **Holding.** The run may make model calls.
//! - **Released.** The run has at least one outstanding call to another
//!   agent ([`RunPermit::child_call`]). It is waiting on its children, not
//!   talking to its model, so its permit goes back to the endpoint — to the
//!   child, if the child runs there. That is what lets a sub-leader and its
//!   child share a budget-1 endpoint: the sub-leader is not holding the one
//!   slot its child needs.
//! - **Reacquiring.** The call that brings the outstanding count to zero
//!   ([`ChildCall::finish`]) waits in the endpoint's queue before its result
//!   is delivered. That result is the only thing that can start the run's
//!   next model call, so it is the one place to gate it; results that leave
//!   other calls outstanding are delivered at once.
//!
//! A reacquire the caller gives up on — the run is cancelled or hits its
//! deadline, and the broker drops the call — is a dropped future: it leaves
//! the queue, takes nothing, and the run's result is never delivered, so the
//! run makes no model call.
//!
//! The permit is released for good when the last handle to the run is
//! dropped: the worker reaped, its connection closed.
//!
//! # Queue depth
//!
//! Every wait is a `tracing` event carrying the endpoint, its limit and the
//! depth at that moment, and [`EndpointBudget::queue_depth`] reads the same
//! number live. A budget that is too tight looks like a queue that never
//! empties, and that has to be visible without a debugger.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Instant;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{debug, info};

/// The limit used for an endpoint nothing has been said about.
///
/// One, because the endpoint that needs a budget is a local model server, and
/// a local model server that has not told us otherwise serves one request at
/// a time. Raise it per endpoint, or globally, from settings.
pub const DEFAULT_ENDPOINT_LIMIT: usize = 1;

/// Per-endpoint concurrency limits for the broker's runs.
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
    /// Runs waiting for a permit right now — see [`EndpointBudget::queue_depth`].
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

    /// Wait for a slot on `endpoint`, behind everyone already waiting.
    ///
    /// The returned permit releases the slot when it is dropped, on every
    /// path — the run finishing, the caller hanging up, the broker shutting
    /// down — because a slot that is only released on the happy path is a
    /// budget that shrinks to nothing over a session. Dropping the future
    /// before it resolves leaves the queue and takes nothing.
    pub async fn acquire(&self, endpoint: &str) -> EndpointPermit {
        let live = self.endpoint(endpoint);

        // The common case is an idle endpoint, and it should not pay for the
        // logging the queued case wants. `try_acquire_owned` never jumps the
        // queue: it fails while anyone is waiting.
        if let Ok(permit) = live.semaphore.clone().try_acquire_owned() {
            return EndpointPermit { _permit: permit };
        }

        let queued = QueuedGuard::enter(&live);
        info!(
            endpoint = %endpoint,
            limit = live.limit,
            queue_depth = queued.depth(),
            "A run is waiting for a slot on a busy model endpoint"
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
            "A run was admitted to a model endpoint"
        );
        EndpointPermit { _permit: permit }
    }

    /// How many runs are waiting for a slot on `endpoint` right now.
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
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Arc<Endpoint>>> {
        lock(&self.inner.live)
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

/// Where a run's permit is: see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPermitState {
    /// The run holds its slot and may make model calls.
    Holding,
    /// The run waits on `outstanding` calls and holds no slot.
    Released { outstanding: usize },
    /// The run's last call finished; its result waits for a slot.
    /// `outstanding` is non-zero only if another call started meanwhile.
    Reacquiring { outstanding: usize },
}

enum State {
    /// Holding the slot: dropping the state hands it to the next waiter.
    Holding {
        _slot: EndpointPermit,
    },
    Released {
        outstanding: usize,
    },
    Reacquiring {
        outstanding: usize,
    },
}

impl State {
    fn public(&self) -> RunPermitState {
        match self {
            Self::Holding { .. } => RunPermitState::Holding,
            Self::Released { outstanding } => RunPermitState::Released {
                outstanding: *outstanding,
            },
            Self::Reacquiring { outstanding } => RunPermitState::Reacquiring {
                outstanding: *outstanding,
            },
        }
    }
}

/// One run's endpoint permit, released while the run waits on its calls to
/// other agents and re-acquired, in the endpoint's queue, before the last of
/// them reports back. See the module docs.
///
/// Cheap to clone; the clones are one run. The permit goes back to the
/// endpoint when the last clone is dropped. Whoever only needs to find the
/// run — not keep it alive — holds a [`WeakRunPermit`].
#[derive(Clone)]
pub struct RunPermit {
    inner: Arc<RunInner>,
}

struct RunInner {
    budget: EndpointBudget,
    endpoint: String,
    state: Mutex<State>,
}

impl RunPermit {
    /// Wait for a slot on `endpoint` for a new run, behind everyone already
    /// waiting there. Dropping the future leaves the queue.
    pub async fn acquire(budget: &EndpointBudget, endpoint: &str) -> Self {
        let permit = budget.acquire(endpoint).await;
        Self {
            inner: Arc::new(RunInner {
                budget: budget.clone(),
                endpoint: endpoint.to_string(),
                state: Mutex::new(State::Holding { _slot: permit }),
            }),
        }
    }

    /// A handle that finds this run while it lives, without keeping its
    /// permit alive.
    pub fn downgrade(&self) -> WeakRunPermit {
        WeakRunPermit(Arc::downgrade(&self.inner))
    }

    /// The endpoint this run is metered on.
    pub fn endpoint(&self) -> &str {
        &self.inner.endpoint
    }

    /// Where the run's permit is right now.
    pub fn state(&self) -> RunPermitState {
        self.lock().public()
    }

    /// The run has started a call to another agent. Its first outstanding
    /// call releases its permit; the returned handle is finished when the
    /// call's result is about to be delivered.
    pub fn child_call(&self) -> ChildCall {
        let mut state = self.lock();
        *state = match std::mem::replace(&mut *state, State::Released { outstanding: 0 }) {
            // Dropping the permit here hands the slot to the next waiter.
            State::Holding { .. } => {
                debug!(endpoint = %self.inner.endpoint, "A run released its slot while it waits on a call");
                State::Released { outstanding: 1 }
            }
            State::Released { outstanding } => State::Released {
                outstanding: outstanding + 1,
            },
            State::Reacquiring { outstanding } => State::Reacquiring {
                outstanding: outstanding + 1,
            },
        };
        ChildCall {
            run: Some(self.clone()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        lock(&self.inner.state)
    }
}

impl std::fmt::Debug for RunPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunPermit")
            .field("endpoint", &self.inner.endpoint)
            .field("state", &self.state())
            .finish()
    }
}

/// A [`RunPermit`] that does not keep the run's slot: see
/// [`RunPermit::downgrade`].
#[derive(Debug, Clone)]
pub struct WeakRunPermit(Weak<RunInner>);

impl WeakRunPermit {
    /// The run, if anything still holds it.
    pub fn upgrade(&self) -> Option<RunPermit> {
        self.0.upgrade().map(|inner| RunPermit { inner })
    }
}

/// One outstanding call a run made to another agent.
///
/// [`finish`](Self::finish) it before delivering the call's result. Dropping
/// it unfinished — the call was cancelled, and its result will never be
/// delivered — only counts it off.
#[derive(Debug)]
#[must_use = "finish the call before delivering its result"]
pub struct ChildCall {
    run: Option<RunPermit>,
}

impl ChildCall {
    /// The call's result is ready. If it is the run's last outstanding call,
    /// wait in the endpoint's queue for the run's permit first, so the run's
    /// next model call is metered; otherwise return at once.
    ///
    /// Cancel-safe: dropping the future while it waits leaves the queue,
    /// takes nothing, and leaves the run released.
    pub async fn finish(mut self) {
        let Some(run) = self.run.take() else { return };
        let reacquire = {
            let mut state = run.lock();
            match &mut *state {
                State::Released { outstanding } => {
                    *outstanding = outstanding.saturating_sub(1);
                    if *outstanding == 0 {
                        *state = State::Reacquiring { outstanding: 0 };
                        true
                    } else {
                        false
                    }
                }
                // Another call is already waiting for the slot; it will
                // hold it for the run.
                State::Reacquiring { outstanding } => {
                    *outstanding = outstanding.saturating_sub(1);
                    false
                }
                State::Holding { .. } => false,
            }
        };
        if !reacquire {
            return;
        }

        // If this future is dropped mid-wait, the run goes back to released
        // with whatever is still outstanding.
        struct Abandon<'a>(Option<&'a RunPermit>);
        impl Drop for Abandon<'_> {
            fn drop(&mut self) {
                if let Some(run) = self.0 {
                    let mut state = run.lock();
                    if let State::Reacquiring { outstanding } = *state {
                        debug!(endpoint = %run.inner.endpoint, "A run's caller gave up while it waited for a slot");
                        *state = State::Released { outstanding };
                    }
                }
            }
        }
        let mut abandon = Abandon(Some(&run));

        let permit = run.inner.budget.acquire(&run.inner.endpoint).await;
        abandon.0 = None;
        let mut state = run.lock();
        *state = match *state {
            State::Reacquiring { outstanding: 0 } => State::Holding { _slot: permit },
            // A call started while this one waited: the run is waiting on
            // its children again, so the slot goes straight back.
            State::Reacquiring { outstanding } => State::Released { outstanding },
            // Nothing else moves a run out of reacquiring.
            State::Released { outstanding } => State::Released { outstanding },
            State::Holding { .. } => return,
        };
    }
}

impl Drop for ChildCall {
    fn drop(&mut self) {
        if let Some(run) = self.run.take() {
            let mut state = run.lock();
            if let State::Released { outstanding } | State::Reacquiring { outstanding } =
                &mut *state
            {
                *outstanding = outstanding.saturating_sub(1);
            }
        }
    }
}

/// Counts one waiter for as long as it is alive.
///
/// A guard rather than a pair of `fetch_add`/`fetch_sub` calls because the
/// wait is cancellable: a caller that hangs up drops the future between the
/// two, and a queue depth that only ever goes up is worse than no queue
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

/// A poisoned lock is recovered: every state it guards is whole between
/// statements.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
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

    const ENDPOINT: &str = "http://localhost:11434";

    /// Let spawned tasks reach the semaphore.
    async fn settle() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn a_run_releases_its_slot_while_a_call_is_outstanding() {
        let budget = EndpointBudget::new(1);
        let run = RunPermit::acquire(&budget, ENDPOINT).await;
        assert_eq!(run.state(), RunPermitState::Holding);
        assert_eq!(budget.in_flight(ENDPOINT), 1);

        let call = run.child_call();
        assert_eq!(run.state(), RunPermitState::Released { outstanding: 1 });
        assert_eq!(budget.in_flight(ENDPOINT), 0, "the slot went back");

        // The child takes it, as a child on the same endpoint would.
        let child = RunPermit::acquire(&budget, ENDPOINT).await;
        drop(child);

        call.finish().await;
        assert_eq!(run.state(), RunPermitState::Holding);
        assert_eq!(budget.in_flight(ENDPOINT), 1);
        drop(run);
        assert_eq!(budget.in_flight(ENDPOINT), 0, "the last handle frees it");
    }

    #[tokio::test]
    async fn only_the_last_result_waits_for_the_slot() {
        let budget = EndpointBudget::new(1);
        let run = RunPermit::acquire(&budget, ENDPOINT).await;
        let first = run.child_call();
        let second = run.child_call();
        assert_eq!(run.state(), RunPermitState::Released { outstanding: 2 });

        // Someone else holds the slot now.
        let other = budget.acquire(ENDPOINT).await;
        tokio::time::timeout(Duration::from_millis(200), first.finish())
            .await
            .expect("a result that leaves a call outstanding is delivered at once");
        assert_eq!(run.state(), RunPermitState::Released { outstanding: 1 });

        let last = tokio::spawn(second.finish());
        settle().await;
        assert!(!last.is_finished(), "the last result waits for the slot");
        assert_eq!(run.state(), RunPermitState::Reacquiring { outstanding: 0 });
        assert_eq!(budget.queue_depth(ENDPOINT), 1);

        drop(other);
        tokio::time::timeout(Duration::from_secs(2), last)
            .await
            .expect("it is admitted once the slot frees")
            .unwrap();
        assert_eq!(run.state(), RunPermitState::Holding);
    }

    #[tokio::test]
    async fn re_acquiring_runs_are_served_in_arrival_order() {
        let budget = EndpointBudget::new(1);
        let order = Arc::new(Mutex::new(Vec::new()));
        let held = budget.acquire(ENDPOINT).await;

        let mut waiting = Vec::new();
        for name in ["first", "second", "third"] {
            // A run that released its slot for one call, now finishing.
            let run = RunPermit {
                inner: Arc::new(RunInner {
                    budget: budget.clone(),
                    endpoint: ENDPOINT.to_string(),
                    state: Mutex::new(State::Released { outstanding: 1 }),
                }),
            };
            let call = ChildCall {
                run: Some(run.clone()),
            };
            let order = Arc::clone(&order);
            waiting.push(tokio::spawn(async move {
                call.finish().await;
                lock(&order).push(name);
                run
            }));
            settle().await;
        }
        assert_eq!(budget.queue_depth(ENDPOINT), 3);

        drop(held);
        for task in waiting {
            let run = tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .expect("every waiter is served")
                .unwrap();
            // Hand the slot on to the next in line.
            drop(run);
        }
        assert_eq!(*lock(&order), ["first", "second", "third"]);
    }

    #[tokio::test]
    async fn an_abandoned_reacquire_takes_nothing() {
        let budget = EndpointBudget::new(1);
        let run = RunPermit::acquire(&budget, ENDPOINT).await;
        let call = run.child_call();
        let other = budget.acquire(ENDPOINT).await;

        let waiting = tokio::spawn(call.finish());
        settle().await;
        assert_eq!(budget.queue_depth(ENDPOINT), 1);
        waiting.abort();
        let _ = waiting.await;

        assert_eq!(budget.queue_depth(ENDPOINT), 0, "it left the queue");
        assert_eq!(run.state(), RunPermitState::Released { outstanding: 0 });
        drop(other);
        settle().await;
        assert_eq!(
            budget.in_flight(ENDPOINT),
            0,
            "and the freed slot stays free"
        );
    }

    #[tokio::test]
    async fn a_weak_handle_does_not_keep_the_slot() {
        let budget = EndpointBudget::new(1);
        let run = RunPermit::acquire(&budget, ENDPOINT).await;
        let weak = run.downgrade();
        assert!(weak.upgrade().is_some());
        drop(run);
        assert!(weak.upgrade().is_none());
        assert_eq!(budget.in_flight(ENDPOINT), 0);
    }

    #[tokio::test]
    async fn a_dropped_call_is_counted_off_without_a_wait() {
        let budget = EndpointBudget::new(1);
        let run = RunPermit::acquire(&budget, ENDPOINT).await;
        let first = run.child_call();
        let second = run.child_call();
        drop(first);
        assert_eq!(run.state(), RunPermitState::Released { outstanding: 1 });
        second.finish().await;
        assert_eq!(run.state(), RunPermitState::Holding);
    }

    #[tokio::test]
    async fn a_call_started_during_a_reacquire_sends_the_slot_straight_back() {
        let budget = EndpointBudget::new(1);
        let run = RunPermit::acquire(&budget, ENDPOINT).await;
        let first = run.child_call();
        let other = budget.acquire(ENDPOINT).await;
        let waiting = tokio::spawn(first.finish());
        settle().await;

        let second = run.child_call();
        assert_eq!(run.state(), RunPermitState::Reacquiring { outstanding: 1 });
        drop(other);
        tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(run.state(), RunPermitState::Released { outstanding: 1 });
        assert_eq!(budget.in_flight(ENDPOINT), 0);
        second.finish().await;
        assert_eq!(run.state(), RunPermitState::Holding);
    }
}
