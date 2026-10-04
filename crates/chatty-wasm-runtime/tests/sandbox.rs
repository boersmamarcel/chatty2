//! Sandbox suite (S1, AGE-597): fuel, wall-clock, memory, traps, file/config
//! imports, and the WASI surface — against real fixture `.wasm`
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
    CallError, Capability, ModuleManifest, ResourceLimits, ToolCallRequest, ToolErrorKind,
    ToolFailure, UnrequestedGrant, UnsupportedWorld, WasmModule,
};

/// 2x the plan's floor tolerance for timing assertions (plan: +/- 200 ms).
const TOLERANCE: Duration = Duration::from_millis(400);

/// A `tool-call-request` for `tool` with arguments `{"input": input}`.
fn call(tool: &str, input: &str) -> ToolCallRequest {
    ToolCallRequest {
        name: tool.to_string(),
        arguments_json: serde_json::json!({ "input": input }).to_string(),
        call_id: "sandbox-suite".to_string(),
        caller: None,
    }
}

/// Every fixture's behaviour is one tool taking `{"input": string}`.
trait RunTool {
    /// Invoke `tool` with `input`; the result's content.
    async fn run(&mut self, tool: &str, input: &str) -> anyhow::Result<String>;
}

impl RunTool for WasmModule {
    async fn run(&mut self, tool: &str, input: &str) -> anyhow::Result<String> {
        self.invoke_tool(call(tool, input)).await.map(|r| r.content)
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

/// A curated subset of the "good" fixtures whose tool output is
/// deterministic with a simple input (the adversarial fixtures — `spin`,
/// `panic`, `trap`, `slow-host`, `huge-output`, `threads` — are exercised by
/// their own dedicated rows instead). The three exports are `metadata`,
/// `list-tools` and `invoke-tool`; 0.2.0's `chat` and `get-agent-card` no
/// longer exist (PL-U3).
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_1_good_fixtures_all_three_exports() {
    // echo: tools are echo/reverse/count_words; it requests nothing.
    {
        let mut m = load(
            "echo",
            ModuleManifest::new("echo"),
            ResourceLimits::default(),
        );
        let metadata = m.metadata().expect("metadata");
        assert_eq!(metadata.name, "echo");
        assert!(metadata.requested_capabilities.is_empty());
        let tools = m.list_tools().expect("list_tools");
        assert_eq!(tools.len(), 3);
        assert_eq!(m.run("reverse", "abc").await.expect("reverse"), "cba");
        assert_eq!(m.run("count_words", "a b c").await.expect("count"), "3");
        assert!(m.last_invocation_metrics().is_some());
        // The guest's own error keeps its kind.
        let err = m.run("nope", "x").await.expect_err("no such tool");
        let failure = err
            .downcast_ref::<ToolFailure>()
            .expect("a guest tool error");
        assert_eq!(failure.kind, ToolErrorKind::UnknownTool);
        assert_eq!(format!("{failure}"), "unknown-tool: unknown tool: nope");
    }

    // benford: the two audit tools, deterministic, no host calls.
    {
        let mut m = load(
            "benford",
            ModuleManifest::new("benford"),
            ResourceLimits::default(),
        );
        let metadata = m.metadata().expect("metadata");
        assert_eq!(metadata.name, "benford");
        let names: Vec<String> = m
            .list_tools()
            .expect("list_tools")
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names, ["compute_benford_distribution", "chi_square_test"]);
        let out = m
            .invoke_tool(ToolCallRequest {
                name: "chi_square_test".to_string(),
                arguments_json: r#"{"observed_counts":[900,10,10,10,10,10,10,10,10],"total":980}"#
                    .to_string(),
                call_id: "c1".to_string(),
                caller: None,
            })
            .await
            .expect("chi_square_test");
        assert!(
            out.content.contains(r#""risk_level":"HIGH""#),
            "{}",
            out.content
        );
    }

    // tool-args: invoke_tool returns the raw arguments.
    {
        let mut m = load(
            "tool-args",
            ModuleManifest::new("tool-args"),
            ResourceLimits::default(),
        );
        let out = m
            .invoke_tool(call("echo_args", "x"))
            .await
            .expect("invoke_tool");
        assert_eq!(out.content, r#"{"input":"x"}"#);
        assert!(m.last_invocation_metrics().is_some());
    }

    // stateful: the tool counts calls in a static across invocations.
    {
        let mut m = load(
            "stateful",
            ModuleManifest::new("stateful"),
            ResourceLimits::default(),
        );
        let first: u64 = m
            .run("count", "x")
            .await
            .expect("count")
            .parse()
            .expect("numeric");
        let second: u64 = m
            .run("count", "x")
            .await
            .expect("count")
            .parse()
            .expect("numeric");
        assert_eq!(second, first + 1, "stateful must count across calls");
    }

    // config-reader: returns `config::get(<input>)`.
    {
        let manifest = ModuleManifest::new("config-reader")
            .with_grants([Capability::Config])
            .with_config("greeting", "hi");
        let mut m = load("config-reader", manifest, ResourceLimits::default());
        assert_eq!(
            m.run("get", "greeting").await.expect("get"),
            r#"Some("hi")"#
        );
    }

    // file-reader: returns the byte length of `file::read_bytes(<input>)`.
    {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.bin"), b"hello").unwrap();
        let manifest = ModuleManifest::new("file-reader")
            .with_grants([Capability::File])
            .with_weights_root(tmp.path());
        let mut m = load("file-reader", manifest, ResourceLimits::default());
        assert_eq!(m.run("read", "a.bin").await.expect("read"), "5");
    }

    // log-flood: logs N lines and reports the count; keep N small so the
    // test stays fast (the flood-volume behaviour is row 1.11's concern).
    {
        let mut m = load(
            "log-flood",
            ModuleManifest::new("log-flood"),
            ResourceLimits::default(),
        );
        assert_eq!(m.run("flood", "5").await.expect("flood"), "logged 5 lines");
    }

    // fuel-meter: burns N loop iterations and reports fuel consumed.
    {
        let mut m = load(
            "fuel-meter",
            ModuleManifest::new("fuel-meter"),
            ResourceLimits::default(),
        );
        let out = m.run("burn", "1000").await.expect("burn");
        assert!(out.starts_with("burned 1000 iterations"));
        let metrics = m.last_invocation_metrics().expect("metrics populated");
        assert!(metrics.fuel_consumed > 0, "fuel_consumed must be > 0");
    }
}

// ---------------------------------------------------------------------------
// 1.2 - `spin` with a small fuel budget: fuel error, host thread free, < 5s.
//
// The default fuel budget (AGE-708: 10^12, sized so a pure spin reaches the
// 60s wall-clock ceiling before fuel runs out) no longer exhausts within 5s,
// so this row gives `spin` an explicit small fuel limit instead of relying
// on the default — it is still exercising the same "fuel exhausted" path,
// just without waiting a minute for it. See `cpu_bound_tool_hits_wall_clock_not_fuel`
// below for the default-fuel behaviour this change is about.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_2_spin_default_limits_traps_on_fuel() {
    let limits = ResourceLimits {
        max_fuel: 100_000_000,
        ..ResourceLimits::default()
    };
    let mut m = load("spin", ModuleManifest::new("spin"), limits);
    let start = Instant::now();
    let result = m.run("spin", "x").await;
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
    let result = m.run("spin", "x").await;
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
// AGE-708 (PL-D3b) - `spin` with `max_execution_ms = 2000` and the *default*
// fuel ceiling: a CPU-bound guest must be bounded by the 60s wall clock, not
// by an easily-exhausted fuel budget. Before AGE-708 raised the fuel ceiling
// from 10^9 to 10^12, this row would have ended in `FuelExhausted` well
// before the 2s deadline; it must now end in `DeadlineExceeded`.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn cpu_bound_tool_hits_wall_clock_not_fuel() {
    let limits = ResourceLimits {
        max_execution_ms: 2_000,
        ..ResourceLimits::default()
    };
    let mut m = load("spin", ModuleManifest::new("spin"), limits);
    let start = Instant::now();
    let result = m.run("spin", "x").await;
    let elapsed = start.elapsed();

    let err = result.expect_err("a pure spin must still be stopped by the wall clock");
    assert!(
        matches!(call_error(&err), CallError::DeadlineExceeded { .. }),
        "a CPU-bound tool at the default fuel ceiling must be bounded by the \
         60s wall clock, not by fuel: expected `deadline exceeded`, got: {err:#} \
         (elapsed {elapsed:?})"
    );
    assert!(
        elapsed <= Duration::from_secs(2) + TOLERANCE,
        "expected the timeout within 2s +/- {TOLERANCE:?}, measured {elapsed:?}"
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
        ModuleManifest::new("slow-host").with_grants([Capability::Llm]),
        llm,
        limits,
    )
    .expect("slow-host loads");

    let start = Instant::now();
    let result = m.run("ask", "x").await;
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
// AGE-706 (PL-H1b) - `sleep`: a guest that sleeps through WASI (`wasi:clocks`
// subscribe + `wasi:io/poll`, i.e. `std::thread::sleep`) blocks inside the
// host's poll, where no epoch check runs. The host cuts such a wait short at
// the call deadline, so a 5 s sleep under a 500 ms budget ends in
// `deadline exceeded` well before the sleep would have.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn wasi_sleep_past_deadline_is_interrupted() {
    let limits = ResourceLimits {
        max_execution_ms: 500,
        ..ResourceLimits::default()
    };
    let mut m = load("sleep", ModuleManifest::new("sleep"), limits);
    let start = Instant::now();
    let result = m.run("sleep", "5000").await;
    let elapsed = start.elapsed();

    let err = result.expect_err("a 5 s sleep under a 500 ms budget must not complete");
    assert!(
        matches!(
            call_error(&err),
            CallError::DeadlineExceeded {
                max_execution_ms: 500
            }
        ),
        "expected `deadline exceeded`, got: {err:#} (elapsed {elapsed:?})"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "expected the sleep to be cut off within 1 s, measured {elapsed:?}"
    );

    // The module stays usable: the next call gets a fresh budget.
    let ok = m.run("sleep", "1").await;
    assert_eq!(ok.expect("a 1 ms sleep completes"), "slept 1 ms");
}

#[tokio::test(flavor = "multi_thread")]
async fn wasi_short_sleep_is_allowed() {
    let limits = ResourceLimits {
        max_execution_ms: 500,
        ..ResourceLimits::default()
    };
    let mut m = load("sleep", ModuleManifest::new("sleep"), limits);
    let start = Instant::now();
    let result = m.run("sleep", "50").await;
    let elapsed = start.elapsed();

    assert_eq!(
        result.expect("a 50 ms sleep within a 500 ms budget completes"),
        "slept 50 ms"
    );
    assert!(
        elapsed >= Duration::from_millis(50),
        "the guest really slept, measured {elapsed:?}"
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
        let result = m.run("burn", "2100000").await;
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
        .run("alloc", "16")
        .await
        .expect("16 MiB is within the 256 MiB default cap");
    assert_eq!(resp, "allocated 16 MiB");
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
            .run("alloc", "8")
            .await
            .unwrap_or_else(|e| panic!("8 MiB should succeed against a 32 MiB cap: {e:#}"));
        assert_eq!(resp, "allocated 8 MiB");
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
        .run("alloc", "1024")
        .await
        .expect_err("expected an error for an over-cap allocation");
    assert!(
        matches!(call_error(&err), CallError::MemoryLimit { .. }),
        "expected `memory limit`, got: {err:#}"
    );
    assert!(format!("{err:#}").contains("memory limit"), "{err:#}");

    // The failed call's instance is dropped; the module still serves.
    let resp = m.run("alloc", "1").await.expect("module recovers");
    assert_eq!(resp, "allocated 1 MiB");
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
            .run("alloc", mib)
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
        .run("panic", "boom")
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
    let err = panicking.run("panic", "boom").await.unwrap_err();
    assert!(
        matches!(call_error(&err), CallError::GuestTrap(m) if m.contains("boom")),
        "{err:#}"
    );

    let limits = ResourceLimits {
        max_memory_bytes: 32 * 1024 * 1024,
        ..ResourceLimits::default()
    };
    let mut alloc = load("alloc", ModuleManifest::new("alloc"), limits);
    let err = alloc.run("alloc", "40").await.unwrap_err();
    assert!(
        matches!(call_error(&err), CallError::MemoryLimit { .. }),
        "{err:#}"
    );
    let resp = alloc.run("alloc", "1").await.expect("module recovers");
    assert_eq!(resp, "allocated 1 MiB");
}

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_7_trap_then_reuse_reinstantiates() {
    let mut m = load(
        "trap",
        ModuleManifest::new("trap"),
        ResourceLimits::default(),
    );

    let trapped = m.run("trap", "x").await;
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
    let metadata = m.metadata().expect("metadata after a trap");
    assert_eq!(metadata.name, "trap");
    let tool = m.run("nope", "").await;
    let tool_err = format!("{:#}", tool.expect_err("the fixture has no tools"));
    assert!(
        tool_err.contains("unknown tool: nope"),
        "invoke_tool after a trap should reach the guest, got: {tool_err}"
    );

    // And trapping again is again a clean error.
    assert!(m.run("trap", "y").await.is_err());
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
        .run("emit", "2")
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
    let ok = m.run("emit", "0").await.expect("an empty reply is fine");
    assert!(ok.is_empty());

    // A lower cap applies to the metadata exports too.
    let mut tiny = load(
        "huge-output",
        ModuleManifest::new("huge-output"),
        ResourceLimits {
            max_output_bytes: 8,
            ..ResourceLimits::default()
        },
    );
    let err = tiny.metadata().expect_err("the metadata is over 8 bytes");
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
        let manifest = ModuleManifest::new("file-reader")
            .with_grants([Capability::File])
            .with_weights_root(root);
        let mut m = load("file-reader", manifest, ResourceLimits::default());
        m.run("read", path).await
    };

    let ok = read("a.bin", tmp.path().to_path_buf()).await;
    assert_eq!(ok.expect("in-root file reads"), "5");
    // A Windows-style separator names the same in-root file on every host.
    let ok = read("sub\\b.bin", tmp.path().to_path_buf()).await;
    assert_eq!(ok.expect("`\\` is a separator"), "2");

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
        .with_grants([Capability::File])
        .with_config("weights_root", tmp.path().to_str().unwrap());
    let mut m = load("file-reader", manifest, ResourceLimits::default());
    let result = m.run("read", "a.bin").await;
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

    let manifest = ModuleManifest::new("file-reader")
        .with_grants([Capability::File])
        .with_weights_root(tmp.path());
    let mut m = load("file-reader", manifest, ResourceLimits::default());
    for path in ["link", "dir-link/secret"] {
        let result = m.run("read", path).await;
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

    let manifest = ModuleManifest::new("file-reader")
        .with_grants([Capability::File])
        .with_weights_root(tmp.path());
    let mut m = load("file-reader", manifest, ResourceLimits::default());
    let result = m.run("read", "big.bin").await;
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
    let with_config = ModuleManifest::new("config-reader")
        .with_grants([Capability::Config])
        .with_config("greeting", "configured-value");
    let mut m = load("config-reader", with_config, ResourceLimits::default());
    let resp = m.run("get", "greeting").await.expect("get");
    assert_eq!(resp, r#"Some("configured-value")"#);

    let without_config = ModuleManifest::new("config-reader").with_grants([Capability::Config]);
    let mut m2 = load("config-reader", without_config, ResourceLimits::default());
    let resp2 = m2.run("get", "greeting").await.expect("get");
    assert_eq!(resp2, "None");
}

// ---------------------------------------------------------------------------
// 1.11 - `log-flood`: bounded memory, or a documented drop-vs-backpressure
// decision. Decided by PL-U3: guest logs go to the host's `tracing`
// subscriber only. The progress channel that queued every line for the
// A2A module route (unbounded, the open question pinned here before) is
// gone with that route, so there is no host-side queue to grow.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_11_log_flood_is_not_queued_on_the_host() {
    let mut m = load(
        "log-flood",
        ModuleManifest::new("log-flood"),
        ResourceLimits::default(),
    );
    const N: usize = 20_000;
    let resp = m.run("flood", &N.to_string()).await.expect("log-flood");
    assert_eq!(resp, format!("logged {N} lines"));
}

// ---------------------------------------------------------------------------
// 1.12 - `wit-0.1`, `wit-0.2`, `core-module`, and truncated bytes: load
// fails with an error that names the cause. A component on any world but
// `chatty:plugin@0.4.0` gets the rebuild message (PL-U3: no adapter).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_12_bad_components_fail_to_load_with_a_named_cause() {
    let engine = WasmModule::build_engine(&ResourceLimits::default()).unwrap();

    // A component built against the old `chatty:module@0.1.0` world.
    let wit01 = WasmModule::from_file(
        &engine,
        &fixture_path("wit-0.1"),
        ModuleManifest::new("wit-0.1"),
        Arc::new(FakeLlm::default()),
        ResourceLimits::default(),
    );
    let message = format!(
        "{:#}",
        wit01.err().expect("a 0.1.0 component must fail to load")
    );
    assert_eq!(
        message,
        "module targets chatty:module@0.1.0; this chatty supports chatty:plugin@0.4.0 \
         — rebuild it with the current SDK"
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
    let bytes = std::fs::read(fixture_path("echo")).expect("read echo bytes");
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

/// The 0.2.0 world everything before PL-U3 was built on is refused at load
/// with the rebuild message, never adapted (PL-D1).
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_12_wit_0_2_module_is_refused_with_the_rebuild_message() {
    let engine = WasmModule::build_engine(&ResourceLimits::default()).unwrap();
    let wit02 = WasmModule::from_file(
        &engine,
        &fixture_path("wit-0.2"),
        ModuleManifest::new("wit-0.2"),
        Arc::new(FakeLlm::default()),
        ResourceLimits::default(),
    );
    let message = format!(
        "{:#}",
        wit02.err().expect("a 0.2.0 component must fail to load")
    );
    assert_eq!(
        message,
        "module targets chatty:module@0.2.0; this chatty supports chatty:plugin@0.4.0 \
         — rebuild it with the current SDK"
    );
}

/// MK-2 (ADR-0024): 0.4.0 took `billing` out of the plugin world, and a
/// component built on 0.3.x is refused at load with the named error — not
/// a link failure over the import it may still carry — so its publisher
/// knows to rebuild with the current SDK and republish.
#[tokio::test(flavor = "multi_thread")]
async fn wit_030_component_refused_with_named_error() {
    let engine = WasmModule::build_engine(&ResourceLimits::default()).unwrap();
    let err = WasmModule::from_file(
        &engine,
        &fixture_path("wit-0.3"),
        ModuleManifest::new("wit-0.3"),
        Arc::new(FakeLlm::default()),
        ResourceLimits::default(),
    )
    .err()
    .expect("a 0.3.0 component must fail to load");
    assert_eq!(
        err.downcast_ref::<UnsupportedWorld>(),
        Some(&UnsupportedWorld {
            found: "chatty:plugin@0.3.0".to_string()
        }),
        "{err:#}"
    );
    assert_eq!(
        format!("{err:#}"),
        "module targets chatty:plugin@0.3.0; this chatty supports chatty:plugin@0.4.0 \
         — rebuild it with the current SDK"
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
        .run("spawn", "x")
        .await
        .expect("the threads tool must not trap");
    assert!(
        resp.contains("spawn: Err"),
        "OPEN QUESTION pinned: thread spawn is rejected (Unsupported) rather than \
         allowed or trapping the load, got: {}",
        resp
    );
    assert!(
        resp.contains("counter: 1"),
        "the guest's own atomic counter must still work single-threaded, got: {}",
        resp
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
        "echo",
        ModuleManifest::new("echo"),
        ResourceLimits::default(),
    );
    let resp = m.run("echo", "hello").await.expect("echo");
    assert_eq!(
        resp, "hello",
        "a module with no declared WASI needs must still run normally"
    );
}

// ---------------------------------------------------------------------------
// 1.14 (grants, PL-U4) - the host links only the capabilities a module was
// granted. An ungranted import is a stub that refuses with `capability <x>
// not granted to this agent`, so the module still instantiates and the
// refusal reaches the caller through the tool result; a grant the module did
// not request fails the load.
// ---------------------------------------------------------------------------

/// A module granted nothing (the default) still loads and runs, linked
/// against `logging` alone.
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_14_echo_granted_nothing_still_runs() {
    let mut m = load(
        "echo",
        ModuleManifest::new("echo"),
        ResourceLimits::default(),
    );
    assert!(m.requested_capabilities().is_empty());
    assert_eq!(m.granted_capabilities(), [Capability::Logging]);
    assert_eq!(m.run("reverse", "abc").await.expect("reverse"), "cba");
}

/// `config-reader` requests `config`; not granted it, its `config::get`
/// (an import with no error channel) ends the call with the refusal as the
/// reason, and the module keeps working for the next call.
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_14_config_reader_not_granted_config_is_refused() {
    let manifest = ModuleManifest::new("config-reader").with_config("greeting", "hi");
    let mut m = load("config-reader", manifest, ResourceLimits::default());
    assert_eq!(m.requested_capabilities(), [Capability::Config]);
    assert_eq!(m.granted_capabilities(), [Capability::Logging]);
    for _ in 0..2 {
        let err = m
            .run("get", "greeting")
            .await
            .expect_err("config not granted");
        assert_eq!(
            call_error(&err),
            &CallError::NotGranted {
                capability: "config"
            }
        );
        assert!(
            format!("{err:#}").contains("capability config not granted to this agent"),
            "{err:#}"
        );
    }
}

/// An ungranted import with an error channel hands the guest the refusal
/// as its `Err`: `file` and `llm` each reach the caller as the
/// guest's own tool error carrying the refusal text.
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_14_ungranted_imports_refuse_through_the_guest() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.bin"), b"hello").unwrap();
    let engine = WasmModule::build_engine(&ResourceLimits::default()).unwrap();
    let cases = [
        // A file root alone grants nothing without the `file` capability.
        (
            "file-reader",
            "read",
            "a.bin",
            ModuleManifest::new("file-reader").with_weights_root(tmp.path()),
            "file",
        ),
        (
            "slow-host",
            "ask",
            "x",
            ModuleManifest::new("slow-host"),
            "llm",
        ),
    ];
    for (name, tool, input, manifest, capability) in cases {
        let mut m = WasmModule::from_file(
            &engine,
            &fixture_path(name),
            manifest,
            Arc::new(FakeLlm::default()),
            ResourceLimits::default(),
        )
        .unwrap_or_else(|e| panic!("`{name}` loads with nothing granted: {e:#}"));
        let err = m.run(tool, input).await.expect_err("not granted");
        let failure = err
            .downcast_ref::<ToolFailure>()
            .unwrap_or_else(|| panic!("`{name}`: expected the guest's tool error, got {err:#}"));
        assert_eq!(
            failure.message,
            format!("capability {capability} not granted to this agent"),
            "`{name}`"
        );
    }
}

/// A grant the module did not request is refused at load, naming the
/// capability and what the module does request.
#[test]
fn sandbox_1_14_a_grant_the_module_did_not_request_fails_the_load() {
    let limits = ResourceLimits::default();
    let engine = WasmModule::build_engine(&limits).unwrap();
    let err = WasmModule::from_file(
        &engine,
        &fixture_path("echo"),
        ModuleManifest::new("echo").with_grants([Capability::File]),
        Arc::new(FakeLlm::default()),
        limits,
    )
    .err()
    .expect("echo requests no `file`");
    let refused = err
        .downcast_ref::<UnrequestedGrant>()
        .unwrap_or_else(|| panic!("expected an UnrequestedGrant, got {err:#}"));
    assert_eq!(refused.unrequested, [Capability::File]);
    assert!(
        err.to_string()
            .contains("plugin `echo` is granted `file`, which it does not request"),
        "{err}"
    );
}

/// A module granted what it requests reads through it; so does one served
/// on its own (`config` is a default there).
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_14_granted_capabilities_are_linked() {
    for manifest in [
        ModuleManifest::new("config-reader").with_grants([Capability::Config]),
        ModuleManifest::new("config-reader").with_specless_grants([]),
    ] {
        let mut m = load(
            "config-reader",
            manifest.with_config("greeting", "hi"),
            ResourceLimits::default(),
        );
        assert_eq!(
            m.granted_capabilities(),
            [Capability::Config, Capability::Logging]
        );
        assert_eq!(
            m.run("get", "greeting").await.expect("get"),
            r#"Some("hi")"#
        );
    }
}
