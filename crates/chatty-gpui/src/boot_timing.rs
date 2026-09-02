// Cold-start timing checkpoints (AGE-164).
//
// Boot rankings were previously reasoned from code order, not measured. This module
// gives each boot milestone a named, monotonic timestamp relative to process start so
// before/after comparisons (AGE-161/162/163) have real numbers instead of guesses.
//
// Each checkpoint fires once (by name) and logs its elapsed time since
// `mark_process_start()`; a final `log_summary()` call rolls every recorded checkpoint
// into one line for easy grepping.

use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static PROCESS_START: OnceLock<Instant> = OnceLock::new();
static CHECKPOINTS: Mutex<Vec<(&'static str, u128)>> = Mutex::new(Vec::new());

/// Record the process start time. Call once, as early as possible in `main()`.
pub fn mark_process_start() {
    PROCESS_START.get_or_init(Instant::now);
}

/// Record a named boot checkpoint with its elapsed time (ms) since `mark_process_start()`.
/// Idempotent per name — later calls with the same `name` are ignored, so call sites that
/// can fire more than once (e.g. mutually exclusive startup branches) don't need their own
/// "only once" guard.
pub fn checkpoint(name: &'static str) {
    let Some(start) = PROCESS_START.get() else {
        return;
    };
    let elapsed_ms = start.elapsed().as_millis();
    let Ok(mut points) = CHECKPOINTS.lock() else {
        return;
    };
    if points.iter().any(|(n, _)| *n == name) {
        return;
    }
    points.push((name, elapsed_ms));
    tracing::info!(checkpoint = name, elapsed_ms, "Boot checkpoint");
}

/// Log a single-line summary of every checkpoint recorded so far. Call once boot is
/// considered complete (the app becomes ready to send).
pub fn log_summary() {
    let Ok(points) = CHECKPOINTS.lock() else {
        return;
    };
    let summary = points
        .iter()
        .map(|(name, ms)| format!("{name}={ms}ms"))
        .collect::<Vec<_>>()
        .join(", ");
    tracing::info!(boot_summary = %summary, "Boot timing summary");
}
