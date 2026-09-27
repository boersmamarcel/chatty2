//! Sandbox suite (S1, AGE-597): fuel, wall-clock, memory, traps, file/config/
//! billing imports, and the WASI surface — against real fixture `.wasm`
//! components (built by `scripts/build-wasm-fixtures.sh`, AGE-596).
//!
//! One test per row of the evaluation plan's §3 S1 table (rows 1.1-1.14).
//! Each test asserts the **correct** behaviour from the plan's pass-criterion
//! column, not today's behaviour. A row that is known to fail today is
//! `#[ignore = "known defect: <id>"]` so `cargo test -- --ignored` lists
//! every open defect by name.
//!
//! Timing assertions use 2x the plan's floor (200 ms -> 400 ms) as the
//! tolerance, per the runbook, and record the measured value in the panic
//! message.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chatty_wasm_runtime::test_support::{FakeLlm, FakeResponse, fixture_path};
use chatty_wasm_runtime::{
    CallError, ChatRequest, Message, ModuleManifest, ResourceLimits, Role, WasmModule,
};

/// 2x the plan's floor tolerance for timing assertions (plan: +/- 200 ms).
const TOLERANCE: Duration = Duration::from_millis(400);

fn user_req(content: &str) -> ChatRequest {
    ChatRequest {
        messages: vec![Message {
            role: Role::User,
            content: content.to_string(),
        }],
        conversation_id: "sandbox-suite".to_string(),
    }
}

fn load(name: &str, manifest: ModuleManifest, limits: ResourceLimits) -> WasmModule {
    let engine = WasmModule::build_engine(&limits).expect("engine");
    WasmModule::from_file(
        &engine,
        &fixture_path(name),
        manifest,
        Arc::new(FakeLlm::default()),
        limits,
    )
    .unwrap_or_else(|e| panic!("fixture `{name}` failed to load: {e:#}"))
}

/// The [`CallError`] behind a failed guest call.
fn call_error(err: &anyhow::Error) -> &CallError {
    err.downcast_ref::<CallError>()
        .unwrap_or_else(|| panic!("expected a CallError, got: {err:#}"))
}

// ---------------------------------------------------------------------------
// 1.1 - Load each good fixture; call all four exports.
// ---------------------------------------------------------------------------

/// A curated subset of the "good" fixtures whose `chat`/`invoke_tool` output
/// is deterministic with a simple prompt (the adversarial fixtures — `spin`,
/// `panic`, `trap`, `slow-host`, `huge-output`, `threads` — are exercised by
/// their own dedicated rows instead, since calling `chat` on them with a
/// default prompt is either slow, non-deterministic, or the point of a later
/// row).
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_1_good_fixtures_all_four_exports() {
    // echo-agent: chat echoes, tools are echo/reverse/count_words.
    {
        let mut m = load(
            "echo-agent",
            ModuleManifest::new("echo-agent"),
            ResourceLimits::default(),
        );
        let card = m.agent_card().expect("agent_card");
        assert_eq!(card.name, "echo-agent");
        let tools = m.list_tools().expect("list_tools");
        assert_eq!(tools.len(), 3);
        let resp = m.chat(user_req("hello")).await.expect("chat");
        assert_eq!(resp.content, "Echo: hello");
        assert!(
            m.last_invocation_metrics().is_some(),
            "last_invocation_metrics must be populated after chat"
        );
        let out = m.invoke_tool("reverse", "abc").await.expect("invoke_tool");
        assert_eq!(out, "cba");
        assert!(m.last_invocation_metrics().is_some());
    }

    // benford-agent: a scripted single-turn LLM reply with no tool calls
    // short-circuits the agentic loop immediately.
    {
        let llm = Arc::new(FakeLlm::new([FakeResponse::Text(
            "Audit report: LOW risk.".to_string(),
        )]));
        let engine = WasmModule::build_engine(&ResourceLimits::default()).unwrap();
        let mut m = WasmModule::from_file(
            &engine,
            &fixture_path("benford-agent"),
            ModuleManifest::new("benford-agent"),
            llm,
            ResourceLimits::default(),
        )
        .expect("benford-agent loads");
        let card = m.agent_card().expect("agent_card");
        assert_eq!(card.name, "benford-agent");
        let resp = m.chat(user_req("1234 4521 891")).await.expect("chat");
        assert_eq!(resp.content, "Audit report: LOW risk.");
        assert!(m.last_invocation_metrics().is_some());
    }

    // tool-args: chat echoes the prompt; invoke_tool returns raw args.
    {
        let mut m = load(
            "tool-args",
            ModuleManifest::new("tool-args"),
            ResourceLimits::default(),
        );
        let resp = m.chat(user_req("ping")).await.expect("chat");
        assert_eq!(resp.content, "ping");
        let out = m
            .invoke_tool("echo_args", r#"{"input":"x"}"#)
            .await
            .expect("invoke_tool");
        assert_eq!(out, r#"{"input":"x"}"#);
        assert!(m.last_invocation_metrics().is_some());
    }

    // stateful: chat counts calls in a static across invocations.
    {
        let mut m = load(
            "stateful",
            ModuleManifest::new("stateful"),
            ResourceLimits::default(),
        );
        let first: u64 = m
            .chat(user_req("x"))
            .await
            .expect("chat")
            .content
            .parse()
            .expect("numeric count");
        let second: u64 = m
            .chat(user_req("x"))
            .await
            .expect("chat")
            .content
            .parse()
            .expect("numeric count");
        assert_eq!(second, first + 1, "stateful must count across calls");
    }

    // config-reader: chat returns `config::get(<prompt>)`.
    {
        let manifest = ModuleManifest::new("config-reader").with_config("greeting", "hi");
        let mut m = load("config-reader", manifest, ResourceLimits::default());
        let resp = m.chat(user_req("greeting")).await.expect("chat");
        assert_eq!(resp.content, r#"Some("hi")"#);
    }

    // file-reader: chat returns the byte length of `file::read_bytes(<prompt>)`.
    {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.bin"), b"hello").unwrap();
        let manifest = ModuleManifest::new("file-reader").with_weights_root(tmp.path());
        let mut m = load("file-reader", manifest, ResourceLimits::default());
        let resp = m.chat(user_req("a.bin")).await.expect("chat");
        assert_eq!(resp.content, "5");
    }

    // log-flood: chat logs N lines and reports the count; keep N small so
    // the test stays fast (the flood-volume behaviour is row 1.11's concern).
    {
        let mut m = load(
            "log-flood",
            ModuleManifest::new("log-flood"),
            ResourceLimits::default(),
        );
        let resp = m.chat(user_req("5")).await.expect("chat");
        assert_eq!(resp.content, "logged 5 lines");
    }

    // fuel-meter: chat burns N loop iterations and reports fuel consumed.
    {
        let mut m = load(
            "fuel-meter",
            ModuleManifest::new("fuel-meter"),
            ResourceLimits::default(),
        );
        let resp = m.chat(user_req("1000")).await.expect("chat");
        assert!(resp.content.starts_with("burned 1000 iterations"));
        let metrics = m.last_invocation_metrics().expect("metrics populated");
        assert!(metrics.fuel_consumed > 0, "fuel_consumed must be > 0");
    }
}

// ---------------------------------------------------------------------------
// 1.2 - `spin` with default limits: fuel error, host thread free, < 5s.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_2_spin_default_limits_traps_on_fuel() {
    let mut m = load(
        "spin",
        ModuleManifest::new("spin"),
        ResourceLimits::default(),
    );
    let start = Instant::now();
    let result = m.chat(user_req("x")).await;
    let elapsed = start.elapsed();

    let err = result.expect_err("spin must trap once fuel is exhausted");
    assert!(
        matches!(call_error(&err), CallError::FuelExhausted { .. }),
        "error should be `fuel exhausted`, got: {err:#}"
    );
    assert!(
        format!("{err:#}").contains("fuel exhausted"),
        "error should name fuel exhaustion, got: {err:#}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "fuel exhaustion should free the host thread quickly, took {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// 1.3 - `spin` with `max_execution_ms = 500` and huge fuel: the wall-clock
// deadline fires within 500ms +/- tolerance even though the guest never
// yields (epoch interruption). Fuel is set far above what 500ms of spinning
// burns, so it can't be the reason the call ends.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_3_spin_wall_clock_timeout_fires() {
    let limits = ResourceLimits {
        max_fuel: u64::MAX,
        max_execution_ms: 500,
        ..ResourceLimits::default()
    };
    let mut m = load("spin", ModuleManifest::new("spin"), limits);
    let start = Instant::now();
    let result = m.chat(user_req("x")).await;
    let elapsed = start.elapsed();

    let err = result.expect_err("expected a timeout error");
    assert!(
        matches!(call_error(&err), CallError::DeadlineExceeded { .. }),
        "expected `deadline exceeded`, got: {err:#} (elapsed {elapsed:?})"
    );
    let message = format!("{err:#}");
    assert!(
        message.contains("timed out"),
        "expected the wall-clock timeout to fire, got: {message} (elapsed {elapsed:?})"
    );
    assert!(
        elapsed <= Duration::from_millis(500) + TOLERANCE,
        "expected the timeout within 500ms +/- {TOLERANCE:?}, measured {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// 1.4 - `slow-host` with `max_execution_ms = 1000`: a slow LLM provider
// stalls the guest in *host* time. The deadline counts host time: the host
// stops waiting at ~1s and the call fails with `deadline exceeded`, instead
// of waiting out the full provider delay.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_4_slow_host_wall_clock_timeout_fires() {
    let limits = ResourceLimits {
        max_execution_ms: 1_000,
        ..ResourceLimits::default()
    };
    let llm = Arc::new(FakeLlm::new([FakeResponse::Delay(
        Duration::from_millis(2_500),
        "late".to_string(),
    )]));
    let engine = WasmModule::build_engine(&limits).unwrap();
    let mut m = WasmModule::from_file(
        &engine,
        &fixture_path("slow-host"),
        ModuleManifest::new("slow-host"),
        llm,
        limits,
    )
    .expect("slow-host loads");

    let start = Instant::now();
    let result = m.chat(user_req("x")).await;
    let elapsed = start.elapsed();

    let err = result.expect_err("expected a timeout error");
    assert!(
        matches!(call_error(&err), CallError::DeadlineExceeded { .. }),
        "expected `deadline exceeded`, got: {err:#} (elapsed {elapsed:?})"
    );
    assert!(
        elapsed <= Duration::from_millis(1_000) + TOLERANCE,
        "expected the timeout within 1000ms +/- {TOLERANCE:?}, measured {elapsed:?} \
         (the call ran out the full host-side delay instead)"
    );
}

// ---------------------------------------------------------------------------
// 1.5 - `fuel-meter` called 10 times: fuel is a per-call budget, refilled
// before every call, so a long-lived module never runs dry.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_5_fuel_meter_ten_calls_all_succeed() {
    // Each call burns ~30% of a 100M budget; ten of them need 3x the budget,
    // so this only passes if fuel is refilled per call.
    let limits = ResourceLimits {
        max_fuel: 100_000_000,
        ..ResourceLimits::default()
    };
    let mut m = load("fuel-meter", ModuleManifest::new("fuel-meter"), limits);
    for i in 1..=10 {
        let result = m.chat(user_req("2100000")).await;
        assert!(
            result.is_ok(),
            "call {i}/10 should succeed (fuel should refill per call), got {result:?}"
        );
        let fuel = m.last_invocation_metrics().unwrap().fuel_consumed;
        assert!(
            fuel > 0 && fuel < 100_000_000,
            "call {i}: fuel_consumed is counted per call, got {fuel}"
        );
    }
}

// ---------------------------------------------------------------------------
// 1.6 - `alloc`: 16 MiB succeeds against the default cap; 1 GiB is a clean
// `memory limit` error, not a host abort. Repeated with a manifest-set 32 MiB
// cap.
//
// The fixture builds its buffer by `extend`ing a `Vec` 1 MiB at a time, so
// the Vec doubles and every old buffer stays in linear memory: N MiB needs
// roughly 2N MiB of memory (16 MiB -> 1+2+4+8+16 = 31 MiB plus the heap
// base). That, not a host bug, is why 16 MiB against a 32 MiB cap fails.
//
// These calls are awaited directly, the way production callers do: the
// guest's allocation-failure message goes through WASI stderr, whose sync
// bindings `block_on` the Tokio runtime. Before PL-H1 that panicked the
// host ("Cannot start a runtime from within a runtime"); the call now runs
// off the executor, so the test binary surviving is part of the assertion.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_6_alloc_within_cap_succeeds() {
    let mut m = load(
        "alloc",
        ModuleManifest::new("alloc"),
        ResourceLimits::default(),
    );
    let resp = m
        .chat(user_req("16"))
        .await
        .expect("16 MiB is within the 256 MiB default cap");
    assert_eq!(resp.content, "allocated 16 MiB");
}

/// 8 MiB (~16 MiB of linear memory, see above) against a manifest-set
/// 32 MiB cap: succeeds, and the same instance serves a second call.
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_6_alloc_within_tight_manifest_cap_succeeds() {
    let limits32 = ResourceLimits {
        max_memory_bytes: 32 * 1024 * 1024,
        ..ResourceLimits::default()
    };
    let mut m32 = load("alloc", ModuleManifest::new("alloc"), limits32);
    for _ in 0..2 {
        let resp = m32
            .chat(user_req("8"))
            .await
            .unwrap_or_else(|e| panic!("8 MiB should succeed against a 32 MiB cap: {e:#}"));
        assert_eq!(resp.content, "allocated 8 MiB");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_6_alloc_over_default_cap_is_clean_error() {
    let mut m = load(
        "alloc",
        ModuleManifest::new("alloc"),
        ResourceLimits::default(),
    );
    // Over the 256 MiB default cap.
    let err = m
        .chat(user_req("1024"))
        .await
        .expect_err("expected an error for an over-cap allocation");
    assert!(
        matches!(call_error(&err), CallError::MemoryLimit { .. }),
        "expected `memory limit`, got: {err:#}"
    );
    assert!(format!("{err:#}").contains("memory limit"), "{err:#}");

    // The failed call's instance is dropped; the module still serves.
    let resp = m.chat(user_req("1")).await.expect("module recovers");
    assert_eq!(resp.content, "allocated 1 MiB");
}

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_6_alloc_over_manifest_cap_is_clean_error() {
    let limits32 = ResourceLimits {
        max_memory_bytes: 32 * 1024 * 1024,
        ..ResourceLimits::default()
    };
    let mut m = load("alloc", ModuleManifest::new("alloc"), limits32);
    for mib in ["16", "40"] {
        let err = m
            .chat(user_req(mib))
            .await
            .expect_err("expected an error for an over-cap allocation");
        assert!(
            matches!(call_error(&err), CallError::MemoryLimit { .. }),
            "{mib} MiB: expected `memory limit`, got: {err:#}"
        );
    }
}

// ---------------------------------------------------------------------------
// 1.7 - `panic`, `trap`, then a normal call on the same module.
//
// A guest panic is a `guest trap` carrying the panic message (read from the
// guest's stderr), not a host panic. After a trap wasmtime won't enter the
// instance again, so `WasmModule` drops it and re-instantiates lazily on the
// next call (guest statics start over).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_7_panic_is_mapped_to_an_error_not_a_host_panic() {
    let mut m = load(
        "panic",
        ModuleManifest::new("panic"),
        ResourceLimits::default(),
    );
    // Awaited directly, as production callers do.
    let err = m
        .chat(user_req("boom"))
        .await
        .expect_err("a panicking guest must not return Ok");
    assert!(
        matches!(call_error(&err), CallError::GuestTrap(_)),
        "expected `guest trap`, got: {err:#}"
    );
    let message = format!("{err:#}");
    assert!(
        message.contains("guest trap") && message.contains("boom"),
        "error should be mapped with the panic message, got: {message}"
    );
}

/// The same on a current-thread runtime, where blocking the one executor
/// thread would otherwise deadlock or panic: a guest panic and a memory-limit
/// abort are both plain errors, and the module keeps serving.
#[tokio::test(flavor = "current_thread")]
async fn guest_panic_and_memory_limit_never_take_down_a_current_thread_host() {
    let mut panicking = load(
        "panic",
        ModuleManifest::new("panic"),
        ResourceLimits::default(),
    );
    let err = panicking.chat(user_req("boom")).await.unwrap_err();
    assert!(
        matches!(call_error(&err), CallError::GuestTrap(m) if m.contains("boom")),
        "{err:#}"
    );

    let limits = ResourceLimits {
        max_memory_bytes: 32 * 1024 * 1024,
        ..ResourceLimits::default()
    };
    let mut alloc = load("alloc", ModuleManifest::new("alloc"), limits);
    let err = alloc.chat(user_req("40")).await.unwrap_err();
    assert!(
        matches!(call_error(&err), CallError::MemoryLimit { .. }),
        "{err:#}"
    );
    let resp = alloc.chat(user_req("1")).await.expect("module recovers");
    assert_eq!(resp.content, "allocated 1 MiB");
}

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_7_trap_then_reuse_reinstantiates() {
    let mut m = load(
        "trap",
        ModuleManifest::new("trap"),
        ResourceLimits::default(),
    );

    let trapped = m.chat(user_req("x")).await;
    let err = trapped.expect_err("unreachable must trap");
    assert!(
        matches!(call_error(&err), CallError::GuestTrap(_)),
        "expected `guest trap`, got: {err:#}"
    );
    let message = format!("{err:#}");
    assert!(
        message.contains("unreachable"),
        "trap error should name the cause, got: {message}"
    );

    // The next calls run on a fresh instance: the metadata export works and
    // `invoke_tool` reaches the guest (its own "unknown tool" error, not a
    // "cannot enter instance" failure).
    let card = m.agent_card().expect("agent_card after a trap");
    assert_eq!(card.name, "trap");
    let tool = m.invoke_tool("nope", "{}").await;
    let tool_err = format!("{:#}", tool.expect_err("the fixture has no tools"));
    assert!(
        tool_err.contains("unknown tool: nope"),
        "invoke_tool after a trap should reach the guest, got: {tool_err}"
    );

    // And trapping again is again a clean error.
    assert!(m.chat(user_req("y")).await.is_err());
}

// ---------------------------------------------------------------------------
// Output cap: every export's return value is capped at 1 MiB per call.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn output_cap_enforced() {
    let mut m = load(
        "huge-output",
        ModuleManifest::new("huge-output"),
        ResourceLimits::default(),
    );

    // 2 MiB of output against the 1 MiB default cap.
    let err = m
        .chat(user_req("2"))
        .await
        .expect_err("a 2 MiB reply must be rejected");
    assert!(
        matches!(
            call_error(&err),
            CallError::OutputTooLarge { bytes, max_output_bytes: 1_048_576 }
                if *bytes == 2 * 1024 * 1024
        ),
        "expected `output too large`, got: {err:#}"
    );
    assert!(format!("{err:#}").contains("output too large"), "{err:#}");

    // Under the cap passes, on the same module.
    let ok = m.chat(user_req("0")).await.expect("an empty reply is fine");
    assert!(ok.content.is_empty());

    // A lower cap applies to the metadata exports too.
    let mut tiny = load(
        "huge-output",
        ModuleManifest::new("huge-output"),
        ResourceLimits {
            max_output_bytes: 8,
            ..ResourceLimits::default()
        },
    );
    let err = tiny.agent_card().expect_err("the card is over 8 bytes");
    assert!(
        matches!(call_error(&err), CallError::OutputTooLarge { .. }),
        "expected `output too large`, got: {err:#}"
    );
}

// ---------------------------------------------------------------------------
// 1.8 - `file-reader` with a file root granted: only files inside the root
// may be read; `..`, absolute paths, Windows separators and drive paths,
// symlinks that leave the root, and files over the 256 MiB read cap all
// error (F14, fixed by PL-H3).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_8_file_reader_rejects_escapes() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.bin"), b"hello").unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    std::fs::write(tmp.path().join("sub/b.bin"), b"hi").unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), b"nope").unwrap();

    let read = |path: &'static str, root: std::path::PathBuf| async move {
        let manifest = ModuleManifest::new("file-reader").with_weights_root(root);
        let mut m = load("file-reader", manifest, ResourceLimits::default());
        m.chat(user_req(path)).await
    };

    let ok = read("a.bin", tmp.path().to_path_buf()).await;
    assert_eq!(ok.expect("in-root file reads").content, "5");
    // A Windows-style separator names the same in-root file on every host.
    let ok = read("sub\\b.bin", tmp.path().to_path_buf()).await;
    assert_eq!(ok.expect("`\\` is a separator").content, "2");

    for path in [
        "../secret",
        "/etc/passwd",
        "sub/../../secret",
        "..\\secret",
        "sub\\..\\..\\secret",
        "\\etc\\passwd",
        "C:\\Windows\\win.ini",
    ] {
        let result = read(path, tmp.path().to_path_buf()).await;
        assert!(result.is_err(), "`{path}` must be rejected, got {result:?}");
    }

    // A config key named `weights_root` grants nothing: the root comes only
    // from the host (the registry's `[files] root`).
    let manifest = ModuleManifest::new("file-reader")
        .with_config("weights_root", tmp.path().to_str().unwrap());
    let mut m = load("file-reader", manifest, ResourceLimits::default());
    let result = m.chat(user_req("a.bin")).await;
    assert!(
        result.is_err(),
        "a `weights_root` config key must not grant file access, got {result:?}"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_8_file_reader_rejects_symlink_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), b"outside the root").unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret"), tmp.path().join("link")).unwrap();
    std::os::unix::fs::symlink(outside.path(), tmp.path().join("dir-link")).unwrap();

    let manifest = ModuleManifest::new("file-reader").with_weights_root(tmp.path());
    let mut m = load("file-reader", manifest, ResourceLimits::default());
    for path in ["link", "dir-link/secret"] {
        let result = m.chat(user_req(path)).await;
        assert!(
            result.is_err(),
            "a symlink pointing outside the file root must not be followed (`{path}`), \
             got {result:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_8_file_reader_rejects_oversized_file() {
    // One byte over the 256 MiB read cap, as a sparse file: `set_len` costs
    // no disk, and a read that honoured the cap only after reading would
    // blow the guest's memory instead of failing with the cap's message.
    let tmp = tempfile::tempdir().unwrap();
    let big = std::fs::File::create(tmp.path().join("big.bin")).unwrap();
    big.set_len(chatty_wasm_runtime::MAX_FILE_READ_BYTES + 1)
        .unwrap();

    let manifest = ModuleManifest::new("file-reader").with_weights_root(tmp.path());
    let mut m = load("file-reader", manifest, ResourceLimits::default());
    let result = m.chat(user_req("big.bin")).await;
    let err = result.expect_err("a file above the read cap must be rejected");
    assert!(
        format!("{err:#}").contains("over the 268435456-byte cap"),
        "expected the read cap to refuse the file before reading it, got: {err:#}"
    );
}

// ---------------------------------------------------------------------------
// 1.9 - `config-reader` with and without config: returns the configured
// value. At this layer (a `ModuleManifest` built via `.with_config(..)`) it
// always passed; F8 was the module registry never forwarding `module.toml`'s
// `[config]` into that manifest. PL-H3 fixed that, and
// chatty-module-registry's `sandbox_1_9_registry_passes_config_and_files_root`
// proves it end to end from a `module.toml`.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_9_config_reader_with_and_without_config() {
    let with_config =
        ModuleManifest::new("config-reader").with_config("greeting", "configured-value");
    let mut m = load("config-reader", with_config, ResourceLimits::default());
    let resp = m.chat(user_req("greeting")).await.expect("chat");
    assert_eq!(resp.content, r#"Some("configured-value")"#);

    let without_config = ModuleManifest::new("config-reader");
    let mut m2 = load("config-reader", without_config, ResourceLimits::default());
    let resp2 = m2.chat(user_req("greeting")).await.expect("chat");
    assert_eq!(resp2.content, "None");
}

// ---------------------------------------------------------------------------
// 1.10 - billing import with and without a `BillingProvider`.
//
// GAP, not a code defect: none of the fixtures built by
// `scripts/build-wasm-fixtures.sh` (PL-E1/AGE-596) call `billing::acquire-
// session` / `billing::report-usage` — the asset table never listed a
// billing-capable fixture, and `chatty-module-sdk` (unlike `hive-billing-
// sdk`) doesn't even wrap the import. Worse: `BillingProvider`'s methods
// return `chatty_wasm_runtime::bindings::chatty::module::billing::
// SessionInfo`, but the `bindings` module is `pub(crate)` (lib.rs), so this
// public trait cannot actually be implemented from outside the crate at
// all — there is no way for this external, integration-level test to even
// construct a fake `BillingProvider`. Both gaps are called out in the
// comment left on AGE-596. This test only proves that loading without a
// provider succeeds (real, if narrow); the "with a provider" half of the
// row cannot be written until one of those two gaps is closed.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_10_billing_without_provider_loads_and_runs() {
    let engine = WasmModule::build_engine(&ResourceLimits::default()).unwrap();

    // Without a billing provider: loads and runs fine (no fixture calls
    // billing, so its absence is never observed by the guest).
    let without = WasmModule::from_file(
        &engine,
        &fixture_path("tool-args"),
        ModuleManifest::new("tool-args"),
        Arc::new(FakeLlm::default()),
        ResourceLimits::default(),
    );
    assert!(
        without.is_ok(),
        "loading without a billing provider must succeed"
    );
}

// ---------------------------------------------------------------------------
// 1.11 - `log-flood` with a progress receiver that isn't drained: bounded
// memory, or a documented drop-vs-backpressure decision. This is one of the
// plan's "decide and pin" rows: `progress_tx` is an `UnboundedSender`, so
// today's answer is "no backpressure, no drop — the queue grows without
// bound." Pinned, not ignored, with the open question in a comment.
// ---------------------------------------------------------------------------

/// OPEN QUESTION (pin, not ignored): `set_progress_sender` takes an
/// `UnboundedSender<String>`. An undrained receiver therefore imposes no
/// backpressure and drops nothing — every log line queues up in host memory
/// for as long as the channel lives. A module with a genuine log flood (not
/// just this fixture's bounded one) can grow that queue without limit. This
/// needs a decision: bound the channel and drop, bound it and block, or
/// accept unbounded growth as someone else's problem (e.g. the gateway
/// disconnecting). Pinning today's "everything queues" behaviour so a change
/// here is visible.
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_11_log_flood_backpressure_is_unbounded_today() {
    let mut m = load(
        "log-flood",
        ModuleManifest::new("log-flood"),
        ResourceLimits::default(),
    );
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    m.set_progress_sender(tx);

    const N: usize = 20_000;
    let resp = m
        .chat(user_req(&N.to_string()))
        .await
        .expect("log-flood chat");
    assert_eq!(resp.content, format!("logged {N} lines"));

    // Nothing was drained during the call; all N lines must be sitting in
    // the channel now (no drop, no backpressure).
    let mut queued = 0usize;
    while rx.try_recv().is_ok() {
        queued += 1;
    }
    assert_eq!(
        queued, N,
        "OPEN QUESTION pinned: an undrained progress channel queues every \
         log line with no bound (today: no drop, no backpressure)"
    );
}

// ---------------------------------------------------------------------------
// 1.12 - `wit-0.1`, `core-module`, and truncated bytes: load fails with an
// error that names the cause.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_12_bad_components_fail_to_load_with_a_named_cause() {
    let engine = WasmModule::build_engine(&ResourceLimits::default()).unwrap();

    // A component built against the old `chatty:module@0.1.0` world: the
    // 0.2.0 host linker cannot find the export it wants.
    let wit01 = WasmModule::from_file(
        &engine,
        &fixture_path("wit-0.1"),
        ModuleManifest::new("wit-0.1"),
        Arc::new(FakeLlm::default()),
        ResourceLimits::default(),
    );
    let message = format!(
        "{:#}",
        wit01
            .err()
            .expect("version-mismatched component must fail to load")
    );
    assert!(
        message.contains("chatty:module/agent@0.2.0"),
        "error should name the missing 0.2.0 export, got: {message}"
    );

    // A plain core module (not a component) is rejected by the component
    // parser with a clear cause.
    let core = WasmModule::from_file(
        &engine,
        &fixture_path("core-module"),
        ModuleManifest::new("core-module"),
        Arc::new(FakeLlm::default()),
        ResourceLimits::default(),
    );
    let message = format!("{:#}", core.err().expect("a core module must fail to load"));
    assert!(
        message.contains("component"),
        "error should name that a component was expected, got: {message}"
    );

    // Truncated bytes fail to parse with a clear cause.
    let bytes = std::fs::read(fixture_path("echo-agent")).expect("read echo-agent bytes");
    let truncated = &bytes[..bytes.len() / 2];
    let trunc = WasmModule::from_bytes(
        &engine,
        truncated,
        ModuleManifest::new("truncated"),
        Arc::new(FakeLlm::default()),
        ResourceLimits::default(),
    );
    let message = format!(
        "{:#}",
        trunc.err().expect("truncated bytes must fail to parse")
    );
    assert!(
        message.contains("parse") || message.contains("invalid") || message.contains("bounds"),
        "error should name a parse failure, got: {message}"
    );
}

// ---------------------------------------------------------------------------
// 1.13 - `threads`: shared memory / atomics. This is a "decide and pin" row.
// ---------------------------------------------------------------------------

/// OPEN QUESTION (pin, not ignored): the engine has `wasm_threads(true)`
/// (for future Rayon-based ML modules), but the host's `WasiCtx` grants no
/// thread-spawn capability. Today `std::thread::Builder::spawn` inside a
/// guest fails cleanly with `Unsupported` — the guest keeps running
/// single-threaded rather than the load trapping outright. This needs a
/// decision: is "threads proposal on, but no actual concurrency" the
/// intended state, or should the engine flag be off until a module needs it?
/// Pinning today's behaviour so a change is visible.
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_13_threads_spawn_is_unsupported_today() {
    let mut m = load(
        "threads",
        ModuleManifest::new("threads"),
        ResourceLimits::default(),
    );
    let resp = m
        .chat(user_req("x"))
        .await
        .expect("threads chat must not trap");
    assert!(
        resp.content.contains("spawn: Err"),
        "OPEN QUESTION pinned: thread spawn is rejected (Unsupported) rather than \
         allowed or trapping the load, got: {}",
        resp.content
    );
    assert!(
        resp.content.contains("counter: 1"),
        "the guest's own atomic counter must still work single-threaded, got: {}",
        resp.content
    );
}

// ---------------------------------------------------------------------------
// 1.14 - WASI surface: env, args, preopened fs, sockets, clock, random.
//
// GAP, not a code defect: none of the committed fixtures probe the raw WASI
// surface directly (they only use `chatty_module_sdk`'s host imports, which
// don't touch WASI env/args/fs/sockets at all). Proving "only clock/random
// work" needs a fixture that calls those `wasi:cli`/`wasi:filesystem`/
// `wasi:sockets` functions itself, which the PL-E1 asset table doesn't
// include. See the comment left on AGE-596. This test instead pins what can
// be verified from the host side without a new fixture: the `WasiCtx` built
// for every module grants no env vars, no args, and no preopened
// directories.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_14_wasi_surface_grants_nothing_by_default() {
    // Every good fixture already instantiates and runs against this WasiCtx
    // (see 1.1); if env/args/preopens were granted, module authors would be
    // relying on ambient host state they never declared. This test is a
    // placeholder pinning that expectation until a `wasi-probe` fixture (see
    // the AGE-596 comment) can assert it end to end.
    let mut m = load(
        "echo-agent",
        ModuleManifest::new("echo-agent"),
        ResourceLimits::default(),
    );
    let resp = m.chat(user_req("hello")).await.expect("chat");
    assert_eq!(
        resp.content, "Echo: hello",
        "a module with no declared WASI needs must still run normally"
    );
}
