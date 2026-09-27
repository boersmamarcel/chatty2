//! Registry and manifest suite (S2, AGE-598): discovery failures, duplicate
//! names, path escapes, limits reaching the runtime, and reload — against
//! temp module directories built from `target/wasm-fixtures/` (PL-E1/
//! AGE-596), never the user's real data dir.
//!
//! One test per row of the evaluation plan's §3 S2 table (rows 2.1-2.8).
//! Each asserts the **correct** behaviour from the plan's pass-criterion
//! column, not today's behaviour; a row that fails today is
//! `#[ignore = "known defect: <id>"]` so `cargo test -- --ignored` lists
//! every open defect by name. Nothing here changes `chatty-module-registry`
//! source (tests only).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chatty_module_registry::ModuleRegistry;
use chatty_wasm_runtime::test_support::fixture_path;
use chatty_wasm_runtime::{CompletionResponse, LlmProvider, Message, ResourceLimits};

struct NoopLlm;

impl LlmProvider for NoopLlm {
    fn complete(
        &self,
        _model: &str,
        _messages: Vec<Message>,
        _tools: Option<String>,
    ) -> Result<CompletionResponse, String> {
        Err("noop: no test calls chat".to_string())
    }
}

fn registry() -> ModuleRegistry {
    let provider: Arc<dyn LlmProvider> = Arc::new(NoopLlm);
    ModuleRegistry::new(provider, ResourceLimits::default()).expect("registry")
}

/// Stage a module directory at `root/<dir_name>` using the `fixture`'s real
/// `.wasm` bytes (any working fixture will do — these tests exercise
/// discovery and manifest handling, not the module's own behaviour) and a
/// caller-supplied `module.toml` body. Returns the directory path.
fn stage(root: &Path, dir_name: &str, fixture: &str, manifest_toml: &str) -> PathBuf {
    let dir = root.join(dir_name);
    std::fs::create_dir_all(&dir).expect("create module dir");
    std::fs::copy(fixture_path(fixture), dir.join("mod.wasm")).expect("copy fixture wasm");
    std::fs::write(dir.join("module.toml"), manifest_toml).expect("write module.toml");
    dir
}

// ---------------------------------------------------------------------------
// 2.1 - `scan_directory` over good, broken-toml, missing-wasm, remote, and
// nested entries: good loaded; each failure reported (not only logged);
// remote reported distinctly.
// ---------------------------------------------------------------------------

/// `scan_directory` returns `Result<Vec<String>>` — the names of modules it
/// managed to load. It has no way to report *why* an entry failed (today it
/// only `warn!`-logs and skips it), and a successfully "loaded" remote
/// module (which never touches the WASM runtime — `load_from_dir` returns
/// `Ok(name)` for it immediately) is indistinguishable in that `Vec<String>`
/// from a real local module. The plan's pass criterion — failures surfaced
/// on the return value, remote reported distinctly — cannot be expressed
/// against this API at all, per PL-E3's Do 4 ("if the API can't express it,
/// mark red against PL-H3 and say so"); see the comment left on PL-H3
/// (AGE-606). What *can* be asserted here is today's actual return value.
#[test]
#[ignore = "known defect: PL-H3 (AGE-606) — scan_directory cannot report failures or distinguish remote on its return value"]
fn sandbox_2_1_scan_directory_reports_each_failure() {
    let tmp = tempfile::tempdir().unwrap();

    stage(
        tmp.path(),
        "good",
        "tool-args",
        "[module]\nname = \"good\"\nversion = \"1.0.0\"\nwasm = \"mod.wasm\"\n",
    );
    std::fs::create_dir_all(tmp.path().join("broken-toml")).unwrap();
    std::fs::write(
        tmp.path().join("broken-toml/module.toml"),
        "not [ valid toml",
    )
    .unwrap();
    std::fs::create_dir_all(tmp.path().join("missing-wasm")).unwrap();
    std::fs::write(
        tmp.path().join("missing-wasm/module.toml"),
        "[module]\nname = \"missing-wasm\"\nversion = \"1.0.0\"\nwasm = \"nope.wasm\"\n",
    )
    .unwrap();
    stage(
        tmp.path(),
        "remote",
        "tool-args",
        "[module]\nname = \"remote\"\nversion = \"1.0.0\"\nexecution_mode = \"remote\"\n",
    );
    // A module.toml nested two levels down must not be picked up: scan_directory
    // only looks at *immediate* subdirectories of root_dir.
    stage(
        &tmp.path().join("not-scanned"),
        "nested",
        "tool-args",
        "[module]\nname = \"nested\"\nversion = \"1.0.0\"\nwasm = \"mod.wasm\"\n",
    );

    let mut reg = registry();
    let loaded = reg.scan_directory(tmp.path()).expect("scan_directory");

    // What the plan actually wants: every failure named on the return value,
    // and "remote" distinguishable from a real local load. Neither is true
    // today — `loaded` is just `["good", "remote"]` (order notwithstanding),
    // indistinguishable from each other, with broken-toml and missing-wasm
    // silently absent instead of reported.
    assert!(
        loaded
            .iter()
            .any(|e| e.contains("broken-toml") && e.contains("error")),
        "a broken-toml entry's failure should be on the return value, got {loaded:?}"
    );
    assert!(
        loaded
            .iter()
            .any(|e| e.contains("missing-wasm") && e.contains("error")),
        "a missing-wasm entry's failure should be on the return value, got {loaded:?}"
    );
    assert!(
        loaded
            .iter()
            .any(|e| e.contains("remote") && e.contains("skipped")),
        "a remote module should be reported distinctly from a local load, got {loaded:?}"
    );
}

/// What today's `scan_directory` actually does, pinned so the row above's
/// gap is visible against real behaviour: only "good" and "remote" load
/// (both indistinguishable `String` names); the broken and missing-wasm
/// directories are silently skipped; the doubly-nested directory is never
/// visited at all.
#[test]
fn sandbox_2_1_scan_directory_todays_actual_return_value() {
    let tmp = tempfile::tempdir().unwrap();
    stage(
        tmp.path(),
        "good",
        "tool-args",
        "[module]\nname = \"good\"\nversion = \"1.0.0\"\nwasm = \"mod.wasm\"\n",
    );
    std::fs::create_dir_all(tmp.path().join("broken-toml")).unwrap();
    std::fs::write(
        tmp.path().join("broken-toml/module.toml"),
        "not [ valid toml",
    )
    .unwrap();
    std::fs::create_dir_all(tmp.path().join("missing-wasm")).unwrap();
    std::fs::write(
        tmp.path().join("missing-wasm/module.toml"),
        "[module]\nname = \"missing-wasm\"\nversion = \"1.0.0\"\nwasm = \"nope.wasm\"\n",
    )
    .unwrap();
    stage(
        tmp.path(),
        "remote",
        "tool-args",
        "[module]\nname = \"remote\"\nversion = \"1.0.0\"\nexecution_mode = \"remote\"\n",
    );
    stage(
        &tmp.path().join("not-scanned"),
        "nested",
        "tool-args",
        "[module]\nname = \"nested\"\nversion = \"1.0.0\"\nwasm = \"mod.wasm\"\n",
    );

    let mut reg = registry();
    let mut loaded = reg.scan_directory(tmp.path()).expect("scan_directory");
    loaded.sort();
    assert_eq!(
        loaded,
        vec!["good".to_string(), "remote".to_string()],
        "today: only the two loadable names come back, with no failure detail and no \
         remote/local distinction; the nested module is never visited"
    );
}

// ---------------------------------------------------------------------------
// 2.2 - Two directories declaring the same `name`: a deterministic outcome,
// surfaced as an error, not a silent overwrite.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "known defect: PL-H3 (AGE-606) — duplicate module names overwrite silently"]
fn sandbox_2_2_duplicate_names_are_surfaced_as_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    stage(
        tmp.path(),
        "dir-a",
        "tool-args",
        "[module]\nname = \"dup\"\nversion = \"1.0.0\"\nwasm = \"mod.wasm\"\n",
    );
    stage(
        tmp.path(),
        "dir-b",
        "echo-agent",
        "[module]\nname = \"dup\"\nversion = \"2.0.0\"\nwasm = \"mod.wasm\"\n",
    );

    let mut reg = registry();
    let result = reg.scan_directory(tmp.path());

    // Today: `scan_directory` succeeds and just returns `["dup", "dup"]` (or
    // similar) — the second load silently overwrites the first in the
    // `HashMap`, with no error and no guarantee about which one wins (a
    // `std::fs::read_dir` order is not specified by POSIX).
    assert!(
        result.is_err(),
        "a duplicate module name across two directories must be a surfaced error, \
         got {result:?}"
    );
}

// ---------------------------------------------------------------------------
// 2.3 - `wasm = "../other.wasm"` and an absolute path: rejected.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "known defect: PL-H3 (AGE-606) — module.toml's wasm path is never validated"]
fn sandbox_2_3_wasm_path_escapes_are_rejected() {
    use chatty_module_registry::ModuleManifest;

    let tmp = tempfile::tempdir().unwrap();
    let module_dir = tmp.path().join("victim");
    std::fs::create_dir_all(&module_dir).unwrap();
    let manifest_path = module_dir.join("module.toml");

    let relative_escape = ModuleManifest::from_str(
        "[module]\nname = \"x\"\nversion = \"1.0.0\"\nwasm = \"../other.wasm\"\n",
        &manifest_path,
    );
    assert!(
        relative_escape.is_err(),
        "wasm = \"../other.wasm\" must be rejected, got {relative_escape:?}"
    );

    let absolute = ModuleManifest::from_str(
        "[module]\nname = \"x\"\nversion = \"1.0.0\"\nwasm = \"/etc/passwd\"\n",
        &manifest_path,
    );
    assert!(
        absolute.is_err(),
        "an absolute wasm path must be rejected, got {absolute:?}"
    );
}

/// Pins the concrete severity behind 2.3: `Path::join` replaces its base
/// entirely when the joined component is itself absolute, so
/// `wasm = "/etc/passwd"` doesn't just escape the module directory — the
/// manifest's `wasm_path` becomes literally `/etc/passwd`, with no
/// dependency on `module_dir` at all.
#[test]
fn sandbox_2_3_absolute_wasm_path_replaces_the_module_dir_entirely() {
    use chatty_module_registry::ModuleManifest;

    let manifest = ModuleManifest::from_str(
        "[module]\nname = \"x\"\nversion = \"1.0.0\"\nwasm = \"/etc/passwd\"\n",
        Path::new("/some/module/dir/module.toml"),
    )
    .expect("today: an absolute wasm path parses without error");
    assert_eq!(
        manifest.wasm_path,
        Some(PathBuf::from("/etc/passwd")),
        "today: the module directory is silently discarded for an absolute wasm path"
    );
}

// ---------------------------------------------------------------------------
// 2.4 - Unknown keys / a typo'd `execution_mode = "remtoe"`: rejected or
// warned, not treated as local.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "known defect: PL-H3 (AGE-606) — a typo'd execution_mode silently falls back to local"]
fn sandbox_2_4_typo_execution_mode_is_rejected_or_warned() {
    use chatty_module_registry::ModuleManifest;

    let manifest = ModuleManifest::from_str(
        "[module]\nname = \"x\"\nversion = \"1.0.0\"\nwasm = \"mod.wasm\"\n\
         execution_mode = \"remtoe\"\n",
        Path::new("/some/module/dir/module.toml"),
    );
    // Today this parses fine and `execution_mode` is stored verbatim as
    // "remtoe", which `matches!(.., "remote" | "remote_only")` doesn't
    // match — so the module is silently treated as local, with no warning
    // that the author probably meant "remote".
    assert!(
        manifest.is_err(),
        "a typo'd execution_mode must be rejected (or at least clearly not silently \
         treated as local), got {manifest:?}"
    );
}

#[test]
fn sandbox_2_4_unknown_top_level_keys_are_silently_ignored_today() {
    use chatty_module_registry::ModuleManifest;

    // `[weird]` is not a section this crate's RawManifest knows about.
    let manifest = ModuleManifest::from_str(
        "[module]\nname = \"x\"\nversion = \"1.0.0\"\nwasm = \"mod.wasm\"\n\n\
         [weird]\nsurprise = true\n",
        Path::new("/some/module/dir/module.toml"),
    );
    assert!(
        manifest.is_ok(),
        "today: an unrecognised top-level table is silently accepted, not rejected or warned"
    );
}

// ---------------------------------------------------------------------------
// 2.5 - `[resources]` reach the runtime, verified by fixture behaviour, not
// by reading the struct. The memory half passes; the time half stays red
// until PL-H1 lands (F1: the wall-clock timeout can't fire).
// ---------------------------------------------------------------------------

#[test]
fn sandbox_2_5_resources_memory_reaches_the_runtime() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = stage(
        tmp.path(),
        "alloc",
        "alloc",
        "[module]\nname = \"alloc\"\nversion = \"1.0.0\"\nwasm = \"mod.wasm\"\n\n\
         [resources]\nmax_memory_mb = 32\n",
    );

    let mut reg = registry();
    reg.load(&dir).expect("alloc loads");
    let module = reg.get_mut("alloc").expect("alloc registered");

    // 4 MiB against a 32 MiB manifest cap (an 8x margin) is deliberately
    // generous: the guest's allocator over-requests when it grows (roughly
    // doubling), and a request that lands anywhere near the store's memory
    // limiter — even well inside it, e.g. 8 MiB against a 16 MiB cap — hits
    // a separate PL-H1 defect (AGE-604) where the limiter rejecting *any*
    // grow attempt, including one immediately retried at a smaller size,
    // panics the host (see chatty-wasm-runtime's sandbox suite, row 1.6).
    // This test is only proving the manifest's value reached `WasmModule`,
    // not re-litigating that separate enforcement defect, hence the wide
    // margin.
    let req = chatty_wasm_runtime::ChatRequest {
        messages: vec![chatty_wasm_runtime::Message {
            role: chatty_wasm_runtime::Role::User,
            content: "4".to_string(),
        }],
        conversation_id: "c".to_string(),
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let resp = rt
        .block_on(module.chat(req))
        .expect("4 MiB is within the manifest's 32 MiB cap");
    assert_eq!(resp.content, "allocated 4 MiB");
}

#[test]
#[ignore = "known defect: PL-H1 (AGE-604) — manifest max_execution_ms reaches the runtime but wall-clock enforcement doesn't fire"]
fn sandbox_2_5_resources_time_reaches_the_runtime() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = stage(
        tmp.path(),
        "spin",
        "spin",
        "[module]\nname = \"spin\"\nversion = \"1.0.0\"\nwasm = \"mod.wasm\"\n\n\
         [resources]\nmax_execution_ms = 300\n",
    );

    let mut reg = registry();
    reg.load(&dir).expect("spin loads");
    let module = reg.get_mut("spin").expect("spin registered");

    let req = chatty_wasm_runtime::ChatRequest {
        messages: vec![chatty_wasm_runtime::Message {
            role: chatty_wasm_runtime::Role::User,
            content: "x".to_string(),
        }],
        conversation_id: "c".to_string(),
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let start = std::time::Instant::now();
    let result = rt.block_on(module.chat(req));
    let elapsed = start.elapsed();

    assert!(result.is_err(), "expected a timeout error, got {result:?}");
    let message = format!("{:#}", result.unwrap_err());
    assert!(
        message.contains("timed out"),
        "expected the manifest's 300ms wall-clock limit to fire, got: {message} (elapsed {elapsed:?})"
    );
}

// ---------------------------------------------------------------------------
// 2.6 - `[resources]` absurd values: clamped to a host ceiling. Today they
// are not clamped at all — a huge `max_memory_mb` overflows the u64
// multiplication in `limits_from_manifest` and panics (a debug-build
// finding: in release this would silently wrap to a small/garbage value
// instead — either way, not the documented "decide a ceiling" policy).
// See the comment left on PL-H3 (AGE-606) and PL-D3 (AGE-595, the ceiling
// policy decision this is blocked on).
// ---------------------------------------------------------------------------

#[test]
#[ignore = "known defect: PL-H3 (AGE-606) / PL-D3 (AGE-595) — absurd resource values overflow instead of clamping"]
fn sandbox_2_6_absurd_memory_mb_overflows_instead_of_clamping() {
    let tmp = tempfile::tempdir().unwrap();
    // ~8.6 * 10^12 MiB ("1 TiB" scale is already enough to overflow once
    // multiplied by 1024*1024 twice over inside `limits_from_manifest`).
    let dir = stage(
        tmp.path(),
        "huge-mem",
        "tool-args",
        "[module]\nname = \"huge-mem\"\nversion = \"1.0.0\"\nwasm = \"mod.wasm\"\n\n\
         [resources]\nmax_memory_mb = 9007199254740992\n",
    );

    let mut reg = registry();
    // A synchronous panic (arithmetic overflow), not an async/Tokio one —
    // `catch_unwind` is sufficient and safe here, unlike the WASI-stdout
    // reentrancy panics elsewhere in this suite.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reg.load(&dir)));
    match result {
        Ok(load_result) => {
            // The desired outcome: clamped to some sane ceiling, loading
            // cleanly (or a clean rejection) — either is fine, as long as
            // it isn't a panic.
            let _ = load_result;
        }
        Err(panic) => {
            let message = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            panic!(
                "an absurd max_memory_mb should be clamped to a host ceiling, not panic: {message}"
            );
        }
    }
}

#[test]
fn sandbox_2_6_absurd_execution_ms_loads_uncapped_today() {
    let tmp = tempfile::tempdir().unwrap();
    // i64::MAX ms (~292 million years): the largest value TOML's integer
    // type can represent; u64::MAX itself fails to parse as TOML (a signed
    // 64-bit format), which is its own accidental, non-policy rejection.
    let dir = stage(
        tmp.path(),
        "huge-time",
        "tool-args",
        "[module]\nname = \"huge-time\"\nversion = \"1.0.0\"\nwasm = \"mod.wasm\"\n\n\
         [resources]\nmax_execution_ms = 9223372036854775807\n",
    );

    let mut reg = registry();
    let result = reg.load(&dir);
    assert!(
        result.is_ok(),
        "today: no ceiling is applied to max_execution_ms — an absurd value loads \
         cleanly instead of being clamped, got {result:?}"
    );
}

// ---------------------------------------------------------------------------
// 2.7 - `reload` after replacing the `.wasm` with a broken file: pin
// today's behaviour (the slot is left empty, not reverted).
// ---------------------------------------------------------------------------

/// OPEN QUESTION (pin, not ignored): `ModuleRegistry::reload` removes the
/// existing entry *before* attempting to load the replacement (see its own
/// doc comment: "Leave the slot empty rather than reverting"). If a module
/// author ships a broken update, every caller of `get`/`get_mut` starts
/// getting `None` where they used to get a working module, with no
/// automatic rollback. This needs a decision: keep "fail closed" (today),
/// or revert to the last-good instance so an accidental hot-reload doesn't
/// take a working module offline.
#[test]
fn sandbox_2_7_reload_with_broken_replacement_leaves_the_slot_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = stage(
        tmp.path(),
        "flaky",
        "tool-args",
        "[module]\nname = \"flaky\"\nversion = \"1.0.0\"\nwasm = \"mod.wasm\"\n",
    );

    let mut reg = registry();
    reg.load(&dir).expect("initial load succeeds");
    assert!(reg.get("flaky").is_some());

    // Replace the .wasm with garbage, as a broken hot-reload would.
    std::fs::write(dir.join("mod.wasm"), b"not a component").unwrap();

    let reload_result = reg.reload("flaky");
    assert!(
        reload_result.is_err(),
        "reload of a broken file must report an error"
    );
    assert!(
        reg.get("flaky").is_none(),
        "OPEN QUESTION pinned: today a failed reload leaves the slot empty rather \
         than keeping the last-good instance serving"
    );
}

// ---------------------------------------------------------------------------
// 2.8 - `watch`: whatever it does today.
// ---------------------------------------------------------------------------

/// `watch` starts a filesystem watcher that forwards raw `notify` events
/// over the given channel — nothing in this crate (or, per the plan's
/// survey, anywhere else in the codebase) ever receives from that channel
/// and calls `reload`/`scan_directory` in response. This test proves the
/// watcher itself does fire on a filesystem change; wiring it to anything
/// is absent. A comment asking whether `watch` is kept has been left on
/// PL-U2 (AGE-616), per PL-E3's Do 5.
#[test]
fn sandbox_2_8_watch_fires_on_filesystem_change_but_nothing_consumes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let reg = registry();
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let _watcher = reg.watch(tmp.path(), tx).expect("watch");

    stage(
        tmp.path(),
        "new-module",
        "tool-args",
        "[module]\nname = \"new-module\"\nversion = \"1.0.0\"\nwasm = \"mod.wasm\"\n",
    );

    let rt = tokio::runtime::Runtime::new().unwrap();
    let event = rt.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await
    });
    assert!(
        matches!(event, Ok(Some(_))),
        "the watcher should report the new module.toml being written, got {event:?}"
    );
    // Note what's absent: `reg` itself is untouched by this event — nothing
    // called `scan_directory`/`load`/`reload` on its behalf.
    assert!(
        reg.get("new-module").is_none(),
        "watch() only forwards raw fs events; nothing wires them to a reload"
    );
}
