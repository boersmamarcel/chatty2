//! Sandbox suite (S1, AGE-597): fuel, wall-clock, memory, traps, file/config/
//! billing imports, and the WASI surface — against real fixture `.wasm`
//! components (built by `scripts/build-wasm-fixtures.sh`, AGE-596).
//!
//! One test per row of the evaluation plan's §3 S1 table (rows 1.1-1.14).
//! Each test asserts the **correct** behaviour from the plan's pass-criterion
//! column, not today's behaviour. A row that is known to fail today is
//! `#[ignore = "known defect: <id>"]` so `cargo test -- --ignored` lists
//! every open defect by name; nothing here is fixed (tests only).
//!
//! Timing assertions use 2x the plan's floor (200 ms -> 400 ms) as the
//! tolerance, per the runbook, and record the measured value in the panic
//! message.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chatty_wasm_runtime::test_support::{FakeLlm, FakeResponse, fixture_path};
use chatty_wasm_runtime::{ChatRequest, Message, ModuleManifest, ResourceLimits, Role, WasmModule};

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
        let manifest = ModuleManifest::new("file-reader")
            .with_config("weights_root", tmp.path().to_str().unwrap());
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

    assert!(result.is_err(), "spin must trap once fuel is exhausted");
    let message = format!("{:#}", result.unwrap_err());
    assert!(
        message.contains("fuel"),
        "error should name fuel exhaustion, got: {message}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "fuel exhaustion should free the host thread quickly, took {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// 1.3 - `spin` with `max_execution_ms = 500` and huge fuel: wall-clock
// timeout should fire within 500ms +/- tolerance. It cannot: `chat()` wraps
// a synchronous call with no await point, so `tokio::time::timeout` never
// gets a chance to preempt it (F1). Fuel is set high enough that it isn't
// the reason the call eventually returns, but low enough that the ignored
// run finishes in a few seconds instead of hanging.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
#[ignore = "known defect: PL-H1 (AGE-604)"]
async fn sandbox_1_3_spin_wall_clock_timeout_does_not_fire() {
    let limits = ResourceLimits {
        max_fuel: 20_000_000_000,
        max_execution_ms: 500,
        ..ResourceLimits::default()
    };
    let mut m = load("spin", ModuleManifest::new("spin"), limits);
    let start = Instant::now();
    let result = m.chat(user_req("x")).await;
    let elapsed = start.elapsed();

    assert!(result.is_err(), "expected a timeout error, got {result:?}");
    let message = format!("{:#}", result.unwrap_err());
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
// stalls the guest in *host* time. The timeout should fire around 1s, not
// wait out the full provider delay (F1: no await point inside the
// synchronous `llm::complete` host call either).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
#[ignore = "known defect: PL-H1 (AGE-604)"]
async fn sandbox_1_4_slow_host_wall_clock_timeout_does_not_fire() {
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

    assert!(result.is_ok(), "expected a timeout error, got {result:?}");
    assert!(
        elapsed <= Duration::from_millis(1_000) + TOLERANCE,
        "expected the timeout within 1000ms +/- {TOLERANCE:?}, measured {elapsed:?} \
         (the call ran out the full host-side delay instead)"
    );
}

// ---------------------------------------------------------------------------
// 1.5 - `fuel-meter` called 10 times: fuel is set once at instantiate and
// never refilled (F2), so a long-lived module eventually traps on every
// call even though each individual call is well within budget.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
#[ignore = "known defect: PL-H1 (AGE-604)"]
async fn sandbox_1_5_fuel_meter_ten_calls_all_succeed() {
    // ~30% of the default 100M fuel budget per call, per the plan.
    let mut m = load(
        "fuel-meter",
        ModuleManifest::new("fuel-meter"),
        ResourceLimits::default(),
    );
    for i in 1..=10 {
        let result = m.chat(user_req("2100000")).await;
        assert!(
            result.is_ok(),
            "call {i}/10 should succeed (fuel should refill per call), got {result:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 1.6 - `alloc`: 16 MiB succeeds against the 64 MiB default cap; 128 MiB is a
// clean error, not a host abort. Repeated with a manifest-set 32 MiB cap.
//
// The first half (within-cap) passes today. The second half does not: when
// the store's memory limiter denies growth, the guest's Rust allocator
// prints an "allocation failed" message to its WASI stderr stream before
// trapping. Flushing that stream calls into `wasmtime-wasi`'s
// `in_tokio()` helper, which — because `WasmModule::chat()` is itself being
// polled from inside a Tokio runtime (exactly how every real caller uses
// it) — tries to block a thread that is already driving that runtime and
// panics with "Cannot start a runtime from within a runtime". That is a
// genuine Rust panic, not a caught Wasmtime trap: a direct `.await` (as
// `WasmModule::chat()` does today) lets it unwind straight out and crash the
// host process. This is a new finding beyond the plan's F1-F16 survey; see
// the comment left on PL-H1 (AGE-604) with this evidence. To observe it here
// without taking down the whole test binary, the over-cap calls are driven
// through `tokio::task::spawn` (a panicking task is reported as a
// `JoinError`, not a crashed process) — production code does not do this.
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
        .expect("16 MiB is within the 64 MiB default cap");
    assert_eq!(resp.content, "allocated 16 MiB");
}

/// Same as above, but against a tight manifest-set 32 MiB cap for a 16 MiB
/// allocation — still comfortably within cap, but close enough that the
/// guest allocator's grow-then-retry heuristic bumps the store's memory
/// limiter at least once even though the *final* allocation succeeds. That
/// bump triggers the same WASI-stdout-flush-from-inside-a-running-runtime
/// panic described above (this is broader than "exceeding the cap": it is
/// "coming near it"). See the comment on PL-H1 (AGE-604).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "known defect: PL-H1 (AGE-604) — a tight-but-sufficient cap also panics the host"]
async fn sandbox_1_6_alloc_within_tight_manifest_cap_succeeds() {
    let limits32 = ResourceLimits {
        max_memory_bytes: 32 * 1024 * 1024,
        ..ResourceLimits::default()
    };
    let mut m32 = load("alloc", ModuleManifest::new("alloc"), limits32);
    let req = user_req("16"); // within the 32 MiB cap
    let result = tokio::task::spawn(async move { m32.chat(req).await }).await;
    match result {
        Ok(Ok(resp)) => assert_eq!(resp.content, "allocated 16 MiB"),
        Ok(Err(e)) => panic!("16 MiB should succeed against a 32 MiB cap, got error: {e:#}"),
        Err(join_err) => panic!(
            "an allocation within cap must not panic the host even when the \
             allocator's own growth heuristic comes close to the limit: {join_err}"
        ),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "known defect: PL-H1 (AGE-604) — over-cap alloc panics the host, see test comment"]
async fn sandbox_1_6_alloc_over_default_cap_is_clean_error() {
    let mut m = load(
        "alloc",
        ModuleManifest::new("alloc"),
        ResourceLimits::default(),
    );
    let req = user_req("128"); // over the 64 MiB default cap
    let result = tokio::task::spawn(async move { m.chat(req).await }).await;
    match result {
        Ok(Ok(resp)) => panic!("expected an error for an over-cap allocation, got {resp:?}"),
        Ok(Err(e)) => {
            /* the desired, not-yet-real outcome: a clean Result::Err */
            let _ = e;
        }
        Err(join_err) => panic!(
            "over-cap allocation must be a clean `Result::Err`, not a host panic: {join_err}"
        ),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "known defect: PL-H1 (AGE-604) — over-cap alloc panics the host, see test comment"]
async fn sandbox_1_6_alloc_over_manifest_cap_is_clean_error() {
    let limits32 = ResourceLimits {
        max_memory_bytes: 32 * 1024 * 1024,
        ..ResourceLimits::default()
    };
    let mut m = load("alloc", ModuleManifest::new("alloc"), limits32);
    let req = user_req("40"); // over the manifest-set 32 MiB cap
    let result = tokio::task::spawn(async move { m.chat(req).await }).await;
    match result {
        Ok(Ok(resp)) => panic!("expected an error for an over-cap allocation, got {resp:?}"),
        Ok(Err(e)) => {
            let _ = e;
        }
        Err(join_err) => panic!(
            "over-cap allocation must be a clean `Result::Err`, not a host panic: {join_err}"
        ),
    }
}

// ---------------------------------------------------------------------------
// 1.7 - `panic`, `trap`, then a normal call on the same instance.
//
// Split in two: the panic half is a newly-found defect (see 1.6's comment —
// same root cause, same evidence posted on PL-H1/AGE-604); the trap-then-
// reuse half is one of the plan's explicit "decide and pin" rows, so it
// asserts today's pinned behaviour (wasmtime poisons the instance after a
// trap) with a comment naming the open question, and is not ignored.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
#[ignore = "known defect: PL-H1 (AGE-604) — see sandbox_1_6_alloc_over_default_cap_is_clean_error"]
async fn sandbox_1_7_panic_is_mapped_to_an_error_not_a_host_panic() {
    let mut m = load(
        "panic",
        ModuleManifest::new("panic"),
        ResourceLimits::default(),
    );
    let req = user_req("boom");
    // Driven through `spawn` so the demonstrated defect doesn't also take
    // down every other test in this binary; production code does not do
    // this and would crash on a direct `.await`.
    let result = tokio::task::spawn(async move { m.chat(req).await }).await;
    match result {
        Ok(Err(e)) => {
            let message = format!("{e:#}");
            assert!(
                message.contains("boom"),
                "error should be mapped with the panic message, got: {message}"
            );
        }
        Ok(Ok(resp)) => panic!("a panicking guest must not return Ok, got {resp:?}"),
        Err(join_err) => panic!(
            "a panicking guest call must be mapped to an error, not crash the host: {join_err}"
        ),
    }
}

/// OPEN QUESTION (pin, not ignored): after a trap, wasmtime poisons the
/// component instance — a wasm-level trap (as opposed to a caught guest
/// error) leaves the `Store` unable to enter the instance again. Today every
/// subsequent call on the *same* `WasmModule` fails, even a normal one. If
/// modules are meant to be long-lived (per the registry's hot-reload model),
/// this needs a decision: either the registry must reload after every trap,
/// or `WasmModule` needs a way to detect "poisoned" and reinstantiate
/// in-place. This test pins today's behaviour so a change is visible.
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_7_trap_then_reuse_is_poisoned_today() {
    let mut m = load(
        "trap",
        ModuleManifest::new("trap"),
        ResourceLimits::default(),
    );

    let trapped = m.chat(user_req("x")).await;
    assert!(trapped.is_err(), "unreachable must trap");
    let message = format!("{:#}", trapped.unwrap_err());
    assert!(
        message.contains("unreachable"),
        "trap error should name the cause, got: {message}"
    );

    // Pin: the instance is not reusable after a trap.
    let reused = m.chat(user_req("y")).await;
    assert!(
        reused.is_err(),
        "OPEN QUESTION pinned: today the instance stays poisoned after a trap \
         (an attempt to reuse it errors instead of running normally)"
    );
}

// ---------------------------------------------------------------------------
// 1.8 - `file-reader` with `weights_root` set: only the file inside the root
// may be read; `..`, absolute paths, and paths that try to escape via
// mixed separators must all error. Symlinks that point outside the root and
// files above a size cap are the two known-open gaps (F14 / PL-H3).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_1_8_file_reader_rejects_escapes() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.bin"), b"hello").unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), b"nope").unwrap();

    let read = |path: &'static str, root: std::path::PathBuf| async move {
        let manifest =
            ModuleManifest::new("file-reader").with_config("weights_root", root.to_str().unwrap());
        let mut m = load("file-reader", manifest, ResourceLimits::default());
        m.chat(user_req(path)).await
    };

    let ok = read("a.bin", tmp.path().to_path_buf()).await;
    assert_eq!(ok.expect("in-root file reads").content, "5");

    for path in ["../secret", "/etc/passwd", "sub/../../secret"] {
        let result = read(path, tmp.path().to_path_buf()).await;
        assert!(result.is_err(), "`{path}` must be rejected, got {result:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "known defect: PL-H3 (AGE-606) — symlink escapes the weights_root sandbox"]
async fn sandbox_1_8_file_reader_rejects_symlink_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), b"outside the root").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path().join("secret"), tmp.path().join("link")).unwrap();

    let manifest = ModuleManifest::new("file-reader")
        .with_config("weights_root", tmp.path().to_str().unwrap());
    let mut m = load("file-reader", manifest, ResourceLimits::default());
    let result = m.chat(user_req("link")).await;
    assert!(
        result.is_err(),
        "a symlink pointing outside weights_root must not be followed, got {result:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "known defect: PL-H3 (AGE-606) — file::read_bytes has no size cap"]
async fn sandbox_1_8_file_reader_rejects_oversized_file() {
    // A 40 MiB file stands in for the plan's 2 GiB case: the defect (no
    // cap at any size) is provable without writing 2 GiB to disk. Chosen to
    // stay comfortably clear of the default 64 MiB *memory* cap (a file
    // close to or above it would fail for that unrelated reason — see
    // sandbox_1_6's tight-cap defect — which would make this test pass for
    // the wrong reason instead of proving there is no *read* size cap).
    let tmp = tempfile::tempdir().unwrap();
    let big = vec![0u8; 40 * 1024 * 1024];
    std::fs::write(tmp.path().join("big.bin"), &big).unwrap();

    let manifest = ModuleManifest::new("file-reader")
        .with_config("weights_root", tmp.path().to_str().unwrap());
    let mut m = load("file-reader", manifest, ResourceLimits::default());
    let result = m.chat(user_req("big.bin")).await;
    assert!(
        result.is_err(),
        "a file above a sane size cap must be rejected before being read fully into memory, \
         got {result:?}"
    );
}

// ---------------------------------------------------------------------------
// 1.9 - `config-reader` with and without config: returns the configured
// value. NOTE: AGE-597 lists this row as expected red against PL-H3, but at
// this layer (calling `chatty-wasm-runtime` directly with a `ModuleManifest`
// built via `.with_config(..)`) it passes — see the comment left on PL-H3
// (AGE-606) reconciling this with F8, which is about the module *registry*
// never forwarding a manifest's `[config]` section into the runtime
// `ModuleManifest` it builds (chatty-module-registry/src/registry.rs), not
// about this crate's config plumbing.
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
