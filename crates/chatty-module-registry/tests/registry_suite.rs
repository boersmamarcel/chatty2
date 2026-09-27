//! Registry and manifest suite (S2, AGE-598): discovery failures, duplicate
//! names, path escapes, limits reaching the runtime, and reload — against
//! temp module directories built from `target/wasm-fixtures/` (PL-E1/
//! AGE-596), never the user's real data dir.
//!
//! One test per row of the evaluation plan's §3 S2 table (rows 2.1-2.8).
//! Each asserts the **correct** behaviour from the plan's pass-criterion
//! column, not today's behaviour; a row that fails today is
//! `#[ignore = "known defect: <id>"]` so `cargo test -- --ignored` lists
//! every open defect by name. PL-H3 (AGE-606) fixed rows 2.1-2.4 and 2.6 and
//! added the registry half of row 1.9 (F8) plus the checked-in manifest
//! checks at the end.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chatty_module_registry::{ModuleManifest, ModuleRegistry, ScanReport};
use chatty_wasm_runtime::test_support::{FakeLlm, FakeResponse, fixture_path};
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

/// The directory names of a report's failures, in report order.
fn failed_dirs(report: &ScanReport) -> Vec<String> {
    report
        .failed
        .iter()
        .map(|(dir, _)| dir.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

#[test]
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
    let report = reg.scan_directory(tmp.path()).expect("scan_directory");

    assert_eq!(report.loaded_names(), vec!["good"]);
    assert!(reg.get("good").is_some());
    // Remote is reported distinctly, and never reaches the WASM runtime.
    assert_eq!(report.remote_names(), vec!["remote"]);
    assert!(reg.get("remote").is_none());
    // Each failure is on the return value, with its reason, in dir order.
    assert_eq!(failed_dirs(&report), vec!["broken-toml", "missing-wasm"]);
    assert!(
        report.failed[0].1.contains("invalid TOML"),
        "{:?}",
        report.failed
    );
    assert!(
        report.failed[1].1.contains("nope.wasm"),
        "{:?}",
        report.failed
    );
    // The nested module is never visited.
    assert!(reg.get("nested").is_none());
}

// ---------------------------------------------------------------------------
// 2.2 - Two directories declaring the same `name`: a deterministic outcome,
// surfaced as an error, not a silent overwrite.
// ---------------------------------------------------------------------------

#[test]
fn sandbox_2_2_duplicate_names_are_surfaced_as_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    // Created in reverse order, so a result that followed creation (or
    // `read_dir`) order instead of name order would show.
    stage(
        tmp.path(),
        "dir-b",
        "echo-agent",
        "[module]\nname = \"dup\"\nversion = \"2.0.0\"\nwasm = \"mod.wasm\"\n",
    );
    stage(
        tmp.path(),
        "dir-a",
        "tool-args",
        "[module]\nname = \"dup\"\nversion = \"1.0.0\"\nwasm = \"mod.wasm\"\n",
    );

    let mut reg = registry();
    let report = reg.scan_directory(tmp.path()).expect("scan_directory");

    // Deterministic: the first directory by name wins, the second fails.
    assert_eq!(report.loaded_names(), vec!["dup"]);
    assert_eq!(report.loaded[0].1.version, "1.0.0");
    assert_eq!(reg.manifest("dup").unwrap().version, "1.0.0");
    assert_eq!(failed_dirs(&report), vec!["dir-b"]);
    assert!(
        report.failed[0].1.contains("duplicate module name 'dup'")
            && report.failed[0].1.contains("dir-a"),
        "{:?}",
        report.failed
    );

    // Loading the loser by hand is refused too, and leaves the winner alone.
    let err = reg.load(tmp.path().join("dir-b")).expect_err("duplicate");
    assert!(
        format!("{err:#}").contains("duplicate module name"),
        "{err:#}"
    );
    assert_eq!(reg.manifest("dup").unwrap().version, "1.0.0");

    // A rescan of the same directories reports the same outcome.
    let again = reg.scan_directory(tmp.path()).expect("rescan");
    assert_eq!(again.loaded_names(), vec!["dup"]);
    assert_eq!(failed_dirs(&again), vec!["dir-b"]);
}

// ---------------------------------------------------------------------------
// 2.3 - `wasm = "../other.wasm"` and an absolute path: rejected.
// ---------------------------------------------------------------------------

#[test]
fn sandbox_2_3_wasm_path_escapes_are_rejected() {
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

// ---------------------------------------------------------------------------
// 2.4 - Unknown keys / a typo'd `execution_mode = "remtoe"`: rejected or
// warned, not treated as local.
// ---------------------------------------------------------------------------

#[test]
fn sandbox_2_4_typo_execution_mode_is_rejected_or_warned() {
    let manifest = ModuleManifest::from_str(
        "[module]\nname = \"x\"\nversion = \"1.0.0\"\nwasm = \"mod.wasm\"\n\
         execution_mode = \"remtoe\"\n",
        Path::new("/some/module/dir/module.toml"),
    );
    let err = manifest.expect_err("a typo'd execution_mode must be rejected, not run locally");
    assert!(
        format!("{err:#}").contains("remtoe"),
        "the error should name the bad value: {err:#}"
    );
}

#[test]
fn sandbox_2_4_unknown_keys_are_rejected() {
    for extra in [
        "\n[weird]\nsurprise = true\n",
        "\n[resources]\nmax_memroy_mb = 32\n",
        "pricing_model = \"paid\"\n",
    ] {
        let manifest = ModuleManifest::from_str(
            &format!("[module]\nname = \"x\"\nversion = \"1.0.0\"\nwasm = \"mod.wasm\"\n{extra}"),
            Path::new("/some/module/dir/module.toml"),
        );
        let err = manifest.expect_err(extra);
        assert!(format!("{err:#}").contains("unknown field"), "{err:#}");
    }
}

// ---------------------------------------------------------------------------
// 2.5 - `[resources]` reach the runtime, verified by fixture behaviour, not
// by reading the struct: the manifest's memory cap and wall-clock limit are
// both enforced on a real call.
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
    let module = reg.get("alloc").expect("alloc registered");
    let mut module = module.blocking_lock();

    // The fixture grows a Vec 1 MiB at a time, so N MiB needs roughly
    // 2N MiB of linear memory (see chatty-wasm-runtime's sandbox suite, row
    // 1.6): 4 MiB fits a 32 MiB cap, 40 MiB does not.
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

    let over = chatty_wasm_runtime::ChatRequest {
        messages: vec![chatty_wasm_runtime::Message {
            role: chatty_wasm_runtime::Role::User,
            content: "40".to_string(),
        }],
        conversation_id: "c".to_string(),
    };
    let err = rt
        .block_on(module.chat(over))
        .expect_err("40 MiB is over the manifest's 32 MiB cap");
    assert!(
        format!("{err:#}").contains("memory limit"),
        "expected the manifest's 32 MiB cap to fire, got: {err:#}"
    );
}

/// The time half uses `slow-host` (the guest waits on a host `llm::complete`
/// that takes 2 s) rather than a spinning guest: under the 10⁹-per-call fuel
/// ceiling a pure spin runs out of fuel in well under 300 ms, so only host
/// time can outlast the manifest's limit.
#[test]
fn sandbox_2_5_resources_time_reaches_the_runtime() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = stage(
        tmp.path(),
        "slow-host",
        "slow-host",
        "[module]\nname = \"slow-host\"\nversion = \"1.0.0\"\nwasm = \"mod.wasm\"\n\n\
         [resources]\nmax_execution_ms = 300\n",
    );

    let llm = Arc::new(FakeLlm::new([FakeResponse::Delay(
        Duration::from_secs(2),
        "late".to_string(),
    )]));
    let mut reg = ModuleRegistry::new(llm, ResourceLimits::default()).expect("registry");
    reg.load(&dir).expect("slow-host loads");
    let module = reg.get("slow-host").expect("slow-host registered");
    let mut module = module.blocking_lock();

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
    assert!(
        elapsed < Duration::from_millis(300 + 400),
        "expected the manifest's 300ms limit (+400ms tolerance), measured {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// 2.6 - `[resources]` absurd values: clamped to the host ceilings (PL-D3,
// `chatty_wasm_runtime::MAX_*_CEILING`), with a warning in the scan report.
// ---------------------------------------------------------------------------

#[test]
fn sandbox_2_6_absurd_memory_mb_overflows_instead_of_clamping() {
    let tmp = tempfile::tempdir().unwrap();
    // 2^53 MiB: overflowed the MiB -> bytes multiplication before PL-H1.
    stage(
        tmp.path(),
        "huge-mem",
        "tool-args",
        "[module]\nname = \"huge-mem\"\nversion = \"1.0.0\"\nwasm = \"mod.wasm\"\n\n\
         [resources]\nmax_memory_mb = 9007199254740992\n",
    );

    let mut reg = registry();
    let report = reg.scan_directory(tmp.path()).expect("scan_directory");
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    let (_, manifest) = &report.loaded[0];
    assert_eq!(
        manifest.resources.max_memory_mb,
        chatty_wasm_runtime::MAX_MEMORY_BYTES_CEILING / (1024 * 1024)
    );
    assert!(
        manifest
            .warnings
            .iter()
            .any(|w| w.contains("max_memory_mb") && w.contains("clamped")),
        "the clamp should be a warning in the report, got {:?}",
        manifest.warnings
    );
}

#[test]
fn sandbox_2_6_absurd_execution_ms_is_clamped_with_a_warning() {
    let tmp = tempfile::tempdir().unwrap();
    // i64::MAX ms (~292 million years): the largest value TOML's integer
    // type can represent.
    stage(
        tmp.path(),
        "huge-time",
        "tool-args",
        "[module]\nname = \"huge-time\"\nversion = \"1.0.0\"\nwasm = \"mod.wasm\"\n\n\
         [resources]\nmax_execution_ms = 9223372036854775807\n",
    );

    let mut reg = registry();
    let report = reg.scan_directory(tmp.path()).expect("scan_directory");
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    let (_, manifest) = &report.loaded[0];
    assert_eq!(
        manifest.resources.max_execution_ms,
        chatty_wasm_runtime::MAX_EXECUTION_MS_CEILING
    );
    assert!(
        manifest
            .warnings
            .iter()
            .any(|w| w.contains("max_execution_ms") && w.contains("clamped")),
        "the clamp should be a warning in the report, got {:?}",
        manifest.warnings
    );
}

// ---------------------------------------------------------------------------
// 2.7 - `reload` after replacing the `.wasm` with a broken file: pin
// today's behaviour (the slot is left empty, not reverted).
// ---------------------------------------------------------------------------

/// OPEN QUESTION (pin, not ignored): `ModuleRegistry::reload` removes the
/// existing entry *before* attempting to load the replacement (see its own
/// doc comment: "Leave the slot empty rather than reverting"). If a module
/// author ships a broken update, every caller of `get` starts
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

// ---------------------------------------------------------------------------
// 1.9 (registry half, F8) - `module.toml`'s `[config]` and `[files].root`
// reach the guest: `config::get` returns the manifest's value, and
// `file::read-bytes` reads under the granted root and nowhere else.
// ---------------------------------------------------------------------------

fn chat_text(module: &mut chatty_wasm_runtime::WasmModule, prompt: &str) -> Result<String, String> {
    let req = chatty_wasm_runtime::ChatRequest {
        messages: vec![chatty_wasm_runtime::Message {
            role: chatty_wasm_runtime::Role::User,
            content: prompt.to_string(),
        }],
        conversation_id: "c".to_string(),
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(module.chat(req))
        .map(|r| r.content)
        .map_err(|e| format!("{e:#}"))
}

#[test]
fn sandbox_1_9_registry_passes_config_and_files_root() {
    let tmp = tempfile::tempdir().unwrap();
    stage(
        tmp.path(),
        "config-reader",
        "config-reader",
        "[module]\nname = \"config-reader\"\nversion = \"1.0.0\"\nwasm = \"mod.wasm\"\n\n\
         [config]\ngreeting = \"from module.toml\"\n",
    );
    let files = stage(
        tmp.path(),
        "file-reader",
        "file-reader",
        "[module]\nname = \"file-reader\"\nversion = \"1.0.0\"\nwasm = \"mod.wasm\"\n\n\
         [files]\nroot = \"weights\"\n",
    );
    std::fs::create_dir(files.join("weights")).unwrap();
    std::fs::write(files.join("weights/w.bin"), b"weights").unwrap();

    let mut reg = registry();
    let report = reg.scan_directory(tmp.path()).expect("scan_directory");
    assert!(report.failed.is_empty(), "{:?}", report.failed);

    let config = reg.get("config-reader").expect("config-reader loaded");
    let mut config = config.blocking_lock();
    assert_eq!(
        chat_text(&mut config, "greeting").unwrap(),
        r#"Some("from module.toml")"#
    );
    assert_eq!(chat_text(&mut config, "missing").unwrap(), "None");

    let reader = reg.get("file-reader").expect("file-reader loaded");
    let mut reader = reader.blocking_lock();
    assert_eq!(chat_text(&mut reader, "w.bin").unwrap(), "7");
    // The module's own files (manifest, .wasm) are outside the granted root.
    let escape = chat_text(&mut reader, "../module.toml");
    assert!(escape.is_err(), "got {escape:?}");
}

#[test]
fn sandbox_1_9_no_files_section_grants_no_files() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = stage(
        tmp.path(),
        "file-reader",
        "file-reader",
        "[module]\nname = \"file-reader\"\nversion = \"1.0.0\"\nwasm = \"mod.wasm\"\n\n\
         [config]\nweights_root = \"/\"\n",
    );
    std::fs::write(dir.join("w.bin"), b"x").unwrap();

    let mut reg = registry();
    reg.load(&dir).expect("file-reader loads");
    let reader = reg.get("file-reader").unwrap();
    let mut reader = reader.blocking_lock();
    let err = chat_text(&mut reader, "w.bin").expect_err("no [files] section, no reads");
    assert!(err.contains("no file root"), "{err}");
}

// ---------------------------------------------------------------------------
// Checked-in manifests: every `module.toml` under `modules/` and
// `templates/` parses under the strict format, and the staged fixtures scan
// with only the deliberately unloadable ones failing.
// ---------------------------------------------------------------------------

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn module_tomls(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() && name != "target" && !name.starts_with('.') {
            module_tomls(&path, out);
        } else if name == "module.toml" {
            out.push(path);
        }
    }
}

#[test]
fn checked_in_module_tomls_parse_strictly() {
    let root = workspace_root();
    let mut found = Vec::new();
    module_tomls(&root.join("modules"), &mut found);
    module_tomls(&root.join("templates"), &mut found);
    assert!(
        found.len() >= 18,
        "expected every module.toml, found {found:?}"
    );
    for path in found {
        let manifest = ModuleManifest::from_file(&path)
            .unwrap_or_else(|e| panic!("{}: {e:#}", path.display()));
        assert!(
            manifest.warnings.is_empty(),
            "{}: {:?}",
            path.display(),
            manifest.warnings
        );
    }
}

#[test]
fn staged_fixtures_scan_with_only_the_unloadable_ones_failing() {
    // Any fixture name resolves the staging directory (and panics with the
    // build instructions when it is missing).
    let staged = fixture_path("tool-args")
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf();
    let mut reg = registry();
    let report = reg.scan_directory(&staged).expect("scan_directory");
    // `core-module` is a core module, not a component; `wit-0.1` targets an
    // older WIT package. Both are staged to be refused.
    assert_eq!(
        failed_dirs(&report),
        vec!["core-module", "wit-0.1"],
        "{:?}",
        report.failed
    );
    assert!(report.remote.is_empty());
    assert!(report.loaded_names().contains(&"echo-agent"));

    // The file-reader fixture's `[files] root = "weights"` is staged with it.
    let reader = reg.get("file-reader").expect("file-reader loaded");
    let mut reader = reader.blocking_lock();
    assert_eq!(chat_text(&mut reader, "fixture.bin").unwrap(), "12");
    let config = reg.get("config-reader").expect("config-reader loaded");
    let mut config = config.blocking_lock();
    assert_eq!(
        chat_text(&mut config, "greeting").unwrap(),
        r#"Some("hello from module.toml")"#
    );
}
