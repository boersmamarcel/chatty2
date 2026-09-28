//! `--usage-file <PATH>`: what a headless run spent, as one JSON object a
//! script can read.
//!
//! The file is written when the run ends, however it ends, and rewritten
//! after every model call and every pass while it runs, so a run killed from
//! outside still leaves its latest totals (with `"exit": "running"`). Every
//! write goes to a temporary file in the same directory that is then renamed
//! over `PATH`: a reader sees the previous object or the next, never half of
//! one.
//!
//! # The object (`schema` 1)
//!
//! | Field | Meaning |
//! |-------|---------|
//! | `schema` | `1`; bumped only when a field changes meaning or goes away |
//! | `input_tokens` | Prompt tokens over every model call, cached ones included |
//! | `output_tokens` | Generated tokens over every model call, reasoning included |
//! | `cache_read_tokens` | Prompt tokens served from the provider's cache (part of `input_tokens`), or `null` when no call reported any |
//! | `cache_write_tokens` | Prompt tokens written to the provider's cache (part of `input_tokens`), or `null` when no call reported any |
//! | `reasoning_tokens` | Output tokens spent reasoning (part of `output_tokens`), or `null` when no call reported any |
//! | `model_calls` | Model requests that completed and reported usage |
//! | `tool_calls` | Tool calls the model started |
//! | `tool_calls_failed` | Those that ended in an error |
//! | `turns` | Passes: the task, then every prompt the run sent itself |
//! | `follow_up_passes` | The passes after the task's: stall resumes, retries, nudges, finalizations |
//! | `duration_ms` | Wall-clock time since the run started |
//! | `exit` | `running`, `completed`, `deadline`, `error` or `cancelled` |
//! | `model` | The model identifier the run was started with |
//! | `handoff_invalid_by_role` | A `--team` leader's roles → how many of their handoffs failed their schema (TD-2); absent when none did |
//! | `failure_tags` | `["handoff_misread"]` when a handoff skipped a field its read rules require (TD-2); absent otherwise |
//!
//! The token counts cover the whole run: every pass, and what delegated
//! agents reported spending on its behalf. A request cut off mid-stream (a
//! stall, a dropped connection, a cancel) never reports usage and is not
//! counted. rig reports a missing cache or reasoning count as `0`, so `null`
//! means "none reported", whichever of the two it was.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chatty_core::models::token_usage::{ApiCallUsage, TokenUsage};
use chatty_core::services::handoff::HandoffLedger;
use serde::Serialize;

/// The `schema` the file is written with.
pub const USAGE_FILE_SCHEMA: u32 = 1;

/// How the run ended, or `Running` while it has not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RunExit {
    /// Still going; what a run killed from outside leaves behind.
    Running,
    /// Ended by itself, exit code 0.
    Completed,
    /// Ended with its `--max-duration` budget spent: stopped for it, or
    /// finishing its last pass after the deadline. Exit code 0.
    Deadline,
    /// Ended with an error, non-zero exit code.
    Error,
    /// Interrupted by a signal (SIGINT, SIGTERM, SIGHUP).
    Cancelled,
}

/// The run's running totals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunTotals {
    /// Whole prompts: uncached + cache read + cache written.
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub model_calls: u64,
    pub tool_calls: u64,
    pub tool_calls_failed: u64,
    pub turns: u64,
}

impl RunTotals {
    /// One completed model request.
    pub fn add_call(&mut self, call: &ApiCallUsage) {
        self.input_tokens += u64::from(call.prompt_tokens());
        self.output_tokens += u64::from(call.output_tokens);
        self.cache_read_tokens += u64::from(call.cache_read_tokens);
        self.cache_write_tokens += u64::from(call.cache_write_tokens);
        self.reasoning_tokens += u64::from(call.reasoning_tokens);
        self.model_calls += 1;
    }

    /// What a delegated agent reported spending (its terminal status), or
    /// what one of the agent's plugins spent through `llm::complete` (PL-U2).
    /// Its requests count as model calls; its tool calls are its own.
    pub fn add_delegated(&mut self, usage: &TokenUsage) {
        self.input_tokens += u64::from(usage.prompt_tokens());
        self.output_tokens += u64::from(usage.output_tokens);
        self.cache_read_tokens += u64::from(usage.cache_read_tokens);
        self.cache_write_tokens += u64::from(usage.cache_write_tokens);
        self.reasoning_tokens += usage
            .calls
            .iter()
            .map(|call| u64::from(call.reasoning_tokens))
            .sum::<u64>();
        self.model_calls += if usage.calls.is_empty() {
            u64::from(usage.api_turn_count)
        } else {
            usage.calls.len() as u64
        };
    }

    /// The object the file holds.
    pub fn report(&self, exit: RunExit, duration_ms: u64, model: &str) -> UsageReport {
        let reported = |n: u64| (n > 0).then_some(n);
        UsageReport {
            schema: USAGE_FILE_SCHEMA,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_tokens: reported(self.cache_read_tokens),
            cache_write_tokens: reported(self.cache_write_tokens),
            reasoning_tokens: reported(self.reasoning_tokens),
            model_calls: self.model_calls,
            tool_calls: self.tool_calls,
            tool_calls_failed: self.tool_calls_failed,
            turns: self.turns,
            follow_up_passes: self.turns.saturating_sub(1),
            duration_ms,
            exit,
            model: model.to_string(),
            handoff_invalid_by_role: BTreeMap::new(),
            failure_tags: Vec::new(),
        }
    }
}

/// The JSON object `--usage-file` holds. Field order is the file's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UsageReport {
    pub schema: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub model_calls: u64,
    pub tool_calls: u64,
    pub tool_calls_failed: u64,
    pub turns: u64,
    pub follow_up_passes: u64,
    pub duration_ms: u64,
    pub exit: RunExit,
    pub model: String,
    /// TD-1's scorecard reads these two (TD-2, AGE-693).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub handoff_invalid_by_role: BTreeMap<String, u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failure_tags: Vec<String>,
}

/// Write `report` to `path` through a temporary file in the same directory
/// and a rename, so a reader never sees a partial object.
pub fn write_atomic(path: &Path, report: &UsageReport) -> std::io::Result<()> {
    let mut json = serde_json::to_vec(report).map_err(std::io::Error::other)?;
    json.push(b'\n');
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("the usage file path has no file name"))?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = dir.join(tmp_name);
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

struct State {
    path: Option<PathBuf>,
    model: String,
    started: Instant,
    totals: RunTotals,
    /// A `--team` leader's handoff record (TD-2).
    handoffs: Option<HandoffLedger>,
    /// Set once the run's last word is written; later writes are dropped,
    /// so a checkpoint cannot overwrite how the run ended.
    finished: bool,
    /// Whether a failed write has been reported on stderr; once is enough
    /// for a path that stays unwritable (a directory that does not exist).
    warned: bool,
}

/// The run's totals and, with `--usage-file`, where they go. Cloned into
/// the signal watcher, which writes the `cancelled` object on its own.
#[derive(Clone)]
pub struct UsageRecorder {
    state: Arc<Mutex<State>>,
}

impl Default for UsageRecorder {
    fn default() -> Self {
        Self::new(None, String::new())
    }
}

impl UsageRecorder {
    pub fn new(path: Option<PathBuf>, model: String) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                path,
                model,
                started: Instant::now(),
                totals: RunTotals::default(),
                handoffs: None,
                finished: false,
                warned: false,
            })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The model the run is on, once it is known.
    pub fn set_model(&self, model: String) {
        self.lock().model = model;
    }

    /// Report `ledger`'s handoff counts and failure tags with the totals.
    pub fn set_handoff_ledger(&self, ledger: Option<HandoffLedger>) {
        self.lock().handoffs = ledger;
    }

    /// Whether the run writes a usage file at all.
    pub fn is_enabled(&self) -> bool {
        self.lock().path.is_some()
    }

    /// The run starts now: `duration_ms` counts from here.
    pub fn restart_clock(&self) {
        self.lock().started = Instant::now();
    }

    /// Change the totals.
    pub fn update(&self, f: impl FnOnce(&mut RunTotals)) {
        f(&mut self.lock().totals);
    }

    /// Rewrite the file with the totals so far, `"exit": "running"`.
    pub fn checkpoint(&self) {
        self.write(RunExit::Running, false);
    }

    /// Write how the run ended. The first call wins; later ones are no-ops.
    pub fn finish(&self, exit: RunExit) {
        self.write(exit, true);
    }

    fn write(&self, exit: RunExit, last: bool) {
        // Held across the write, so two writers never interleave and the
        // last word is always the last write.
        let mut state = self.lock();
        let Some(path) = state.path.clone() else {
            return;
        };
        if state.finished {
            return;
        }
        state.finished = last;
        let duration_ms = u64::try_from(state.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut report = state.totals.report(exit, duration_ms, &state.model);
        if let Some(ledger) = state.handoffs.as_ref() {
            report.handoff_invalid_by_role = ledger.invalid_by_role();
            report.failure_tags = ledger.failure_tags();
        }
        if let Err(error) = write_atomic(&path, &report) {
            tracing::warn!(path = %path.display(), %error, "Could not write the usage file");
            // Headless and pipe runs log nowhere, so the one place a
            // caller can see this is stderr; the run itself goes on.
            if !state.warned {
                state.warned = true;
                eprintln!(
                    "warning: could not write --usage-file {}: {error}",
                    path.display()
                );
            }
        }
    }
}

/// Write the `cancelled` object when the process is interrupted, then die of
/// the same signal as it would have without a usage file: the handler is
/// put back to the default and the signal raised again, so the exit status
/// a caller sees does not change. Only installed with `--usage-file`, and
/// only for the signals the process does not already ignore.
#[cfg(unix)]
pub fn watch_for_interrupt(recorder: UsageRecorder) {
    use tokio::signal::unix::{SignalKind, signal};
    if !recorder.is_enabled() {
        return;
    }
    let kinds = [
        (SignalKind::interrupt(), libc::SIGINT),
        (SignalKind::terminate(), libc::SIGTERM),
        (SignalKind::hangup(), libc::SIGHUP),
    ];
    for (kind, signo) in kinds {
        // A signal the process was started ignoring stays ignored: `nohup`
        // sets SIGHUP to SIG_IGN and a non-interactive shell's `&` does the
        // same for SIGINT, and tokio's handler would replace that with one
        // of its own — this watcher would then re-raise the signal under
        // the default action and kill a run that was meant to survive it.
        if signal_is_ignored(signo) {
            continue;
        }
        let Ok(mut stream) = signal(kind) else {
            continue;
        };
        let recorder = recorder.clone();
        tokio::spawn(async move {
            if stream.recv().await.is_some() {
                recorder.finish(RunExit::Cancelled);
                // SAFETY: restoring a signal's default disposition and
                // raising it are async-signal-safe libc calls with no
                // pointers involved; this runs on a normal task, not in a
                // signal handler.
                unsafe {
                    libc::signal(signo, libc::SIG_DFL);
                    libc::raise(signo);
                }
            }
        });
    }
}

/// Whether `signo`'s current disposition is `SIG_IGN`.
#[cfg(unix)]
fn signal_is_ignored(signo: libc::c_int) -> bool {
    // SAFETY: querying a disposition (a null `act`) writes only into the
    // zeroed `sigaction` this function owns.
    unsafe {
        let mut current: libc::sigaction = std::mem::zeroed();
        libc::sigaction(signo, std::ptr::null(), &mut current) == 0
            && current.sa_sigaction == libc::SIG_IGN
    }
}

#[cfg(not(unix))]
pub fn watch_for_interrupt(_recorder: UsageRecorder) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(input: u32, read: u32, write: u32, output: u32, reasoning: u32) -> ApiCallUsage {
        ApiCallUsage {
            turn: 1,
            input_tokens: input,
            cache_read_tokens: read,
            cache_write_tokens: write,
            output_tokens: output,
            reasoning_tokens: reasoning,
            ..Default::default()
        }
    }

    #[test]
    fn the_object_has_every_field_in_order_and_null_for_the_unreported() {
        let mut totals = RunTotals::default();
        totals.add_call(&call(100, 0, 0, 20, 0));
        totals.add_call(&call(30, 0, 0, 5, 0));
        totals.tool_calls = 2;
        totals.tool_calls_failed = 1;
        totals.turns = 2;
        let json = serde_json::to_string(&totals.report(RunExit::Completed, 1234, "qwen")).unwrap();
        assert_eq!(
            json,
            r#"{"schema":1,"input_tokens":130,"output_tokens":25,"cache_read_tokens":null,"cache_write_tokens":null,"reasoning_tokens":null,"model_calls":2,"tool_calls":2,"tool_calls_failed":1,"turns":2,"follow_up_passes":1,"duration_ms":1234,"exit":"completed","model":"qwen"}"#
        );
    }

    #[test]
    fn cached_tokens_are_part_of_the_input_and_reported_ones_are_numbers() {
        let mut totals = RunTotals::default();
        totals.add_call(&call(100, 900, 50, 20, 7));
        let report = totals.report(RunExit::Running, 0, "m");
        assert_eq!(report.input_tokens, 1050);
        assert_eq!(report.cache_read_tokens, Some(900));
        assert_eq!(report.cache_write_tokens, Some(50));
        assert_eq!(report.reasoning_tokens, Some(7));
        assert_eq!(report.follow_up_passes, 0);
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["exit"], "running");
    }

    /// TD-2: a `--team` leader's handoff record goes in the file for TD-1's
    /// scorecard, and nothing is added while it is empty.
    #[test]
    fn a_team_leaders_handoffs_are_reported_with_the_totals() {
        use chatty_core::services::handoff::{HandoffContract, HandoffLedger};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        let recorder = UsageRecorder::new(Some(path.clone()), "m".to_string());
        let coder = HandoffContract {
            role: "coder".to_string(),
            schema: serde_json::json!({}),
        };
        let reviewer = HandoffContract {
            role: "reviewer".to_string(),
            schema: serde_json::json!({ "x-must-be-read": { "coder": ["files_changed"] } }),
        };
        let ledger = HandoffLedger::new([&coder, &reviewer]);
        recorder.set_handoff_ledger(Some(ledger.clone()));

        recorder.checkpoint();
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(json.get("handoff_invalid_by_role").is_none(), "{json}");
        assert!(json.get("failure_tags").is_none(), "{json}");

        ledger.record(
            "coder",
            1,
            Some(&serde_json::json!({ "files_changed": ["a.rs"] })),
        );
        ledger.record("reviewer", 0, Some(&serde_json::json!({ "verdict": "ok" })));
        recorder.finish(RunExit::Completed);
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json["handoff_invalid_by_role"]["coder"], 1);
        assert_eq!(json["failure_tags"][0], "handoff_misread");
    }

    #[test]
    fn a_delegated_agents_usage_counts_its_requests() {
        let mut totals = RunTotals::default();
        totals.add_delegated(&TokenUsage::from_calls(vec![
            call(10, 0, 0, 1, 0),
            call(20, 0, 0, 2, 0),
        ]));
        // A worker that reported only an aggregate.
        totals.add_delegated(&TokenUsage::with_turn_count(5, 5, 3));
        assert_eq!(totals.input_tokens, 35);
        assert_eq!(totals.output_tokens, 8);
        assert_eq!(totals.model_calls, 5);
    }

    #[test]
    fn the_write_replaces_the_file_whole_and_leaves_no_temporary_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        std::fs::write(&path, "stale").unwrap();
        let report = RunTotals::default().report(RunExit::Error, 5, "m");
        write_atomic(&path, &report).unwrap();

        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["exit"], "error");
        assert_eq!(written["schema"], 1);
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("usage.json")]);
    }

    #[test]
    fn the_last_word_is_not_overwritten_by_a_later_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        let recorder = UsageRecorder::new(Some(path.clone()), "m".into());
        recorder.update(|t| t.tool_calls += 1);
        recorder.checkpoint();
        let read = || -> serde_json::Value {
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
        };
        assert_eq!(read()["exit"], "running");
        recorder.finish(RunExit::Deadline);
        recorder.update(|t| t.tool_calls += 1);
        recorder.checkpoint();
        recorder.finish(RunExit::Completed);
        assert_eq!(read()["exit"], "deadline");
        assert_eq!(read()["tool_calls"], 1);
    }

    /// A path whose directory does not exist costs the run nothing but the
    /// file: every write fails quietly and the totals keep counting.
    #[test]
    fn an_unwritable_path_does_not_stop_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing").join("usage.json");
        let recorder = UsageRecorder::new(Some(path.clone()), "m".into());
        recorder.update(|t| t.tool_calls += 1);
        recorder.checkpoint();
        recorder.finish(RunExit::Completed);
        assert!(!path.exists());
        assert!(recorder.lock().warned, "the failure was reported once");
        assert_eq!(recorder.lock().totals.tool_calls, 1);
    }

    /// The watcher leaves an ignored signal ignored (`nohup`, a background
    /// job's SIGINT); this is the check it relies on.
    #[cfg(unix)]
    #[test]
    fn an_ignored_signal_is_recognised() {
        // SIGUSR2 is nobody else's in this test binary.
        assert!(!signal_is_ignored(libc::SIGUSR2));
        // SAFETY: setting and restoring a disposition of a signal no test or
        // runtime in this process handles.
        unsafe {
            libc::signal(libc::SIGUSR2, libc::SIG_IGN);
            assert!(signal_is_ignored(libc::SIGUSR2));
            libc::signal(libc::SIGUSR2, libc::SIG_DFL);
        }
        assert!(!signal_is_ignored(libc::SIGUSR2));
    }

    #[test]
    fn without_a_path_nothing_is_written() {
        let recorder = UsageRecorder::default();
        assert!(!recorder.is_enabled());
        recorder.checkpoint();
        recorder.finish(RunExit::Completed);
    }
}
