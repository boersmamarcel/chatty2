# WASM plugin performance baseline (S7)

**When to read this:** you are doing PL-U2 (AGE-616, in-process plugin
tools), or re-checking PL-H1 (AGE-604, per-call limits) or PL-H4 (AGE-607,
gateway concurrency — already merged during this issue, see §2's post-H4
column), and need a before/after number for the plugin path's cold load,
per-protocol dispatch overhead, concurrency, and a long-running soak.

Recorded by AGE-603 (plugin evaluation plan §3 S7). Benchmarks, not gates:
nothing here blocks anything: it's a baseline to diff future changes against.

## Machine and method

Linux `marcel-beast`, 6.8.0-139-generic, x86_64, 28 cores, 125 GiB RAM,
rustc/cargo 1.98.1. **This box was shared with other agents' builds and a
resident vLLM inference server for the whole session** — `uptime`'s load
average is recorded next to every run below, and it swings from ~17 to ~49
on 28 cores. Absolute numbers are therefore noisy; the pre/post-PL-H1
*deltas* (same code path, same box, minutes apart) are the more trustworthy
signal than any single absolute figure, and the concurrency numbers in
particular should be re-measured on an otherwise-idle box before anyone acts
on the absolute throughput figures.

All fixtures are release-optimized WASM (`wasm32-wasip2`), instantiated with
the fake LLM providers in `chatty_wasm_runtime::test_support` /
`chatty-wasm-runtime`'s own `FakeLlm` (return instantly, no network). The
gateway benches use a real `ProtocolGateway` on an ephemeral loopback port,
no participant socket.

**One-command re-run** (from a fresh checkout):

```bash
rustup target add wasm32-wasip2   # once
bash scripts/build-wasm-fixtures.sh
export CARGO_TARGET_DIR=/some/dir   # keep off a loop-mounted disk
cargo bench -p chatty-protocol-gateway --bench load_time
cargo bench -p chatty-protocol-gateway --bench protocol_overhead
cargo bench -p chatty-protocol-gateway --bench concurrency
# 60-minute soak, run in the background (real wall time):
nohup cargo bench -p chatty-protocol-gateway --bench soak > soak.log 2>&1 &
# shorter soak for a smoke check:
SOAK_SECONDS=300 cargo bench -p chatty-protocol-gateway --bench soak
```

## Three columns: pre-H1, post-H1, post-H4

This issue ran across a busy stretch of the epic: two more milestone-3
issues merged into `main` while it was in progress. Three columns, each on
this branch (`age-603-perf-baseline`), same code except for what `main`
carried at the time:

* **Pre-H1**: chatty2 `main` `8bfa8804` (the branch point before any of the
  below landed) — commit `a7ccc1b7` in this branch.
* **Post-H1**: chatty2 `main` `cf714636` — adds PL-H1 (AGE-604, `7f0abeb1`,
  #941: fuel reset **per call** instead of once at instantiate, guest calls
  moved onto `spawn_blocking`, PL-D3 per-call fuel/output/deadline ceilings),
  PL-H3 (AGE-606, `14dfd77b`, #943: strict `module.toml`, `[config]`/`[files]`
  reach the guest, hardened file sandbox), and AGE-682/683 (`fdfa4ace`, #926:
  usage lines carry model/time). Merge commit `b1b250c4`.
* **Post-H4 (final)**: chatty2 `main` `bdc7a6cd` — adds PL-H4 (AGE-607,
  `baf2f2b1`, #944: **per-module locks** replacing the gateway's single
  global write lock, correct roles/parts/history, protocol flags enforced,
  a loopback-only guard), AGE-605 (`4988b8c5`, #946: plugin `llm::complete`
  routed through chatty-core's provider client instead of its own HTTP
  client), and AGE-613 (`bdc7a6cd`, #947: docs only). Merge commit
  `98ab7342`. This is the tree the PR actually ships.

`load_time` is unaffected by any of these (they change per-call limit
enforcement and gateway locking, not module loading), so it was only run
pre-H1. §3 (concurrency) and §4 (soak) were run pre-H1 and post-H1 only —
PL-U2 (AGE-616) is asked to re-run the whole suite after its own change, and
that run will be the first to reflect PL-H4 there; §2 (protocol overhead) has
all three columns since it is the fastest to re-run and the most directly
affected by both PL-H1 and PL-H4.

## §1 Cold load time

`WasmModule::from_file`: parse + Cranelift-compile a component, no guest
code runs. Median of 20 samples (10 for `load_20_copies`), pre-H1, load avg
20.2/32.4/35.0:

| Module | Size | Cold load (median) |
| -- | -- | -- |
| echo-agent | 97 KB | 17.6 ms |
| benford-agent | 214 KB | 39.4 ms |
| padded (new PL-E1-style fixture, §"Fixtures added") | 5.08 MiB | 28.1 ms |

Load time does not scale with binary size in this range — `benford-agent`
(214 KB) is slower to load than `padded` (5.08 MiB) — consistent with
Cranelift compile time being dominated by code complexity (benford's
agentic tool-calling loop) rather than raw byte count for a component this
small; the 5 MiB fixture is mostly one flat data segment, cheap to validate
and skip over.

Loading 20 copies back to back, same engine, fresh store each time:

| Module | 20 copies (median) | Per copy |
| -- | -- | -- |
| echo-agent | 420.9 ms | 21.0 ms |
| benford-agent | 683.4 ms | 34.2 ms |

Command: `cargo bench -p chatty-protocol-gateway --bench load_time`.

## §2 Per-protocol call overhead

`chat`-shaped (direct `WasmModule::chat` vs the OpenAI route vs A2A
`message/send`) and `invoke_tool`-shaped (direct `WasmModule::invoke_tool`
vs MCP `tools/call`) — OpenAI/A2A have no tool-invocation route of their
own and MCP has no free-form chat route, so these are the honest pairings.
All against `echo-agent`, `"use llm: hello"` / tool `echo`, so `chat`
actually exercises the FakeLlm round trip on every route.

| Path | Pre-H1 (load 22.0/25.6/25.3) | Post-H1 (load 22.0/25.6/25.3) | Post-H4 (load 38.9/37.6/36.5) | Pre-H1 → Post-H1 | Post-H1 → Post-H4 |
| -- | -- | -- | -- | -- | -- |
| direct `WasmModule::chat` | 3.92 µs | 375.3 µs | 48.7 µs | **+96×** | **-89%** |
| OpenAI route | 186.8 µs | 1.20 ms | 134.5 µs | **+6.4×** | **-89%** |
| A2A `message/send` | 213.2 µs | 1.15 ms | 148.8 µs | **+5.4×** | **-88%** |
| direct `WasmModule::invoke_tool` | 1.63 µs | 34.9 µs | 12.1 µs | **+21×** | **-67%** |
| MCP `tools/call` | 169.3 µs | 387.1 µs | 98.7 µs | **+2.3×** | **-70%** |

(Median of 30 samples each; full ranges in the raw logs. The
`direct_wasm_module_invoke_tool` bench's `args` changed from a raw string
to a JSON object between the post-H1 and post-H4 runs, since echo-agent's
`invoke_tool` started requiring `{"input": "..."}` in that window — an
unrelated fixture-contract change, not a perf effect.)

**This is the headline finding, and it moved twice within one issue.**
PL-H1's per-call `spawn_blocking` + epoch-deadline setup added real, dramatic
per-call overhead (+2.3× to +96×) over the old lifetime-fuel model. PL-H4
(landing days later, in the same session) then brought every path back down
*below* the pre-H1 numbers on this box's absolute wall-clock time, likely a
combination of the new per-module locks removing the global-lock contention
this single-caller benchmark wasn't even exercising, and other cleanup in
the same PR — the exact mechanism isn't isolated here since PL-H4 changed
several things in the gateway at once (locking, routing, role/part
handling). PL-U2 (in-process plugin tools, no WASM dispatch at all) should
treat the post-H4 numbers as its floor to beat, not the post-H1 numbers.

Command: `cargo bench -p chatty-protocol-gateway --bench protocol_overhead`.

## §3 Throughput and latency at 1/8/32 concurrent clients

Plain OpenAI-route calls to `echo-agent` with no LLM call (200 requests per
client). Not a criterion microbench (`concurrency.rs`, `harness = false`) —
it reports the percentiles §3 of the plan asks for.

**Pre-H1** (load avg 25.8/35.0/35.9 at start):

| Clients | Modules | Throughput | p50 | p95 | p99 | max |
| -- | -- | -- | -- | -- | -- | -- |
| 1 | one (echo) | 7,871.7 req/s | 0.10 ms | 0.20 ms | 0.28 ms | 1.30 ms |
| 8 | one (echo) | 11,126.9 req/s | 0.33 ms | 1.15 ms | 7.81 ms | 15.58 ms |
| 32 | one (echo) | 17,577.0 req/s | 1.39 ms | 4.00 ms | 7.32 ms | 25.27 ms |
| 1 | two (echo+benford) | 6,571.5 req/s | 0.11 ms | 0.16 ms | 0.84 ms | 3.29 ms |
| 8 | two (echo+benford) | 13,097.3 req/s | 0.42 ms | 1.03 ms | 4.16 ms | 11.40 ms |
| 32 | two (echo+benford) | 13,374.8 req/s | 1.79 ms | 4.96 ms | 10.83 ms | 26.67 ms |

**Post-H1** (load avg 40.2/37.8/36.8 at start — busier box, see caveat above):

| Clients | Modules | Throughput | p50 | p95 | p99 | max |
| -- | -- | -- | -- | -- | -- | -- |
| 1 | one (echo) | 3,537.8 req/s | 0.15 ms | 0.46 ms | 3.07 ms | 3.94 ms |
| 8 | one (echo) | 4,354.4 req/s | 0.89 ms | 6.65 ms | 9.95 ms | 12.66 ms |
| 32 | one (echo) | 5,508.1 req/s | 4.93 ms | 11.84 ms | 16.71 ms | 26.18 ms |
| 1 | two (echo+benford) | 3,736.8 req/s | 0.15 ms | 0.33 ms | 3.69 ms | 6.43 ms |
| 8 | two (echo+benford) | 1,141.1 req/s | 6.31 ms | 16.30 ms | 22.94 ms | 32.41 ms |
| 32 | two (echo+benford) | 956.0 req/s | 32.77 ms | 58.92 ms | 71.93 ms | 83.80 ms |

Two things worth flagging, with the load-average caveat in mind:

* **Two-module throughput collapses under load post-H1** (13,097→1,141 req/s
  at 8 clients; 13,375→956 req/s at 32 clients), while one-module throughput
  merely drops in line with the higher per-call cost from §2. This is
  consistent with F10 (the gateway takes one global write lock per call,
  serializing every module) now mattering more because each call holds that
  lock for `spawn_blocking`'s longer per-call duration — exactly what PL-H4
  is meant to fix. This run's very high concurrent system load (40+) makes
  the absolute drop suspect; the *shape* (two modules markedly worse than
  one, which wasn't true pre-H1) is the part worth re-checking on an idle
  box before treating it as confirmed.
* One-module p50 at 32 clients pre-H1 (1.39 ms) vs post-H1 (4.93 ms) tracks
  the §2 per-call overhead increase almost exactly (~3.5×), as expected.

**Not re-run post-H4** (time-boxed; §2 above already shows PL-H4 landed and
helped, and PL-U2 will re-run this whole suite after its own change) — the
two-module collapse described above is pre/post-H1 only and should be
treated as superseded pending that re-run, not as a live problem.

Command: `cargo bench -p chatty-protocol-gateway --bench concurrency`.

## §4 Soak: 1 call/s to benford-agent, RSS/fd/fuel-exhaustion

Plain binary (`soak.rs`, `harness = false`), one long-lived `WasmModule`,
`benford-agent`'s agentic loop with a scripted instant `FakeLlm` reply (ends
in one turn).

**Pre-H1, full 60 minutes** (started 15:58:14 UTC, `SOAK_SECONDS` unset ⇒
3600 s): **3,601 calls, 0 errors, no fuel-exhaustion error** — RSS flat at
143–151 MB throughout, fd count constant at 11. This is a **negative**
result against F2's naive prediction ("fuel exhaustion errors start once
cumulative fuel runs out"): pre-H1 fuel was a 10⁸-unit *lifetime* budget
set once at instantiate and never refilled, but `benford-agent`'s per-call
fuel cost (§2: ~4 µs direct-call wall time, so very few Wasm instructions
per call once the FakeLlm returns instantly) never came close to exhausting
100,000,000 units over 3,601 calls in an hour. F2 is real (confirmed
separately in AGE-597's sandbox suite with a fixture built to burn fuel
deliberately) but this particular module/workload combination doesn't
trigger it inside an hour — a heavier module, a longer soak, or a higher
call rate would be needed to see it here.

**Post-H1, 7-minute smoke** (`SOAK_SECONDS=420`, started 17:50:13 UTC, load
avg 49.3/45.9/44.4 — box very busy): **421 calls, 0 errors, no
fuel-exhaustion error**, RSS flat at 148 MB, fd count constant at 11. Fully
expected post-H1: fuel resets before every call now, so a fuel-exhaustion
error should never happen regardless of call count.

Raw per-second CSV (elapsed_s, call, ok, error, rss_kib, fd_count) for both
runs is in the PR; not reproduced here in full.

Command:

```bash
nohup cargo bench -p chatty-protocol-gateway --bench soak > soak.log 2>&1 &
```

## Fixtures added for this issue

`modules/fixtures/padded/`: a `chatty-module-sdk` fixture identical in
shape to `echo-agent`'s `chat`, padded to ~5.08 MiB with a `build.rs`-
generated non-uniform data segment (a 5,000,000-iteration LCG loop; done in
`build.rs` rather than a `const fn` in the crate, since that loop trips
rustc's `long_running_const_eval` lint at compile time). Used only for §1's
"a 5 MiB module" cold-load row. Picked up automatically by
`scripts/build-wasm-fixtures.sh`'s existing glob over `modules/fixtures/*/`
with a `Cargo.toml`.

## §5 PL-H1c (AGE-707): cutting the per-call hop — 2026-09-28

PL-H1 ran every `invoke_tool` on `spawn_blocking`, so each call paid two
cross-thread wake-ups (hand the guest to a blocking-pool thread, wake the
awaiting task back). `perf record` over the direct bench shows that hop is
the cost: the guest's own work (`call_raw`, lifting/lowering, `set_fuel`,
`StderrTail::clear`, the echo tool's `logging::log`) is a small CPU slice,
while the wall time goes to parking and unparking threads, which a shared
box under load stretches further.

**Change.** On a multi-threaded Tokio runtime the call now runs in place
under `tokio::task::block_in_place` (the worker's other tasks move to
another thread first, and WASI's sync bindings may `block_on` inside it);
on a current-thread runtime it still hops to `spawn_blocking`, which is
the only option there. A panic is caught with `catch_unwind` on the
in-place path, so it is still `CallError::HostPanic` and the instance is
dropped. Nothing else moved: fuel is refilled, the epoch deadline and the
host-time deadline are armed before every call, the output cap is checked
after it, and a trapped instance is never re-entered. Those per-call
re-arms fit inside the remaining ~1.8 µs and are what the limits are, so
they stay. The sandbox suite, `limits_are_clamped_to_ceilings`
and `output_cap_enforced` are unchanged and green.

**Numbers.** `cargo bench -p chatty-protocol-gateway --bench
protocol_overhead`, `echo` tool, criterion median of 30 samples. Main at
`67c8e759` (post-H1, post-U3) and this branch are the same bench binary
built twice and run **interleaved**, three rounds, on the same box
(i9-10940X, 28 threads; other agents' builds running, load average noted
per round). Best of three:

| Path | Before (main `67c8e759`) | After | Factor |
| -- | -- | -- | -- |
| direct `WasmModule::invoke_tool` (bench thread in `Runtime::block_on`) | 11.63 µs | 1.77 µs | **6.6×** |
| direct `invoke_tool` from a runtime worker (new `_on_worker` bench) | 13.96 µs | 1.65 µs | **8.5×** |
| MCP `tools/call` | 83.3 µs | 90.5 µs | ≈ (HTTP-bound) |

All rounds (median per round; load average 1/5/15 min at the start):

| Round | Build | Load | direct | on worker | MCP |
| -- | -- | -- | -- | -- | -- |
| 1 | before | 10.0/20.3/27.9 | 11.99 µs | 13.96 µs | 83.3 µs |
| 1 | after | 7.6/18.4/27.0 | 1.77 µs | 1.65 µs | 90.5 µs |
| 2 | before | 7.0/16.8/26.0 | 11.63 µs | 16.97 µs | 123.0 µs |
| 2 | after | 11.3/16.3/25.4 | 1.93 µs | 1.79 µs | 126.4 µs |
| 3 | before | 14.0/16.4/25.0 | 14.67 µs | 17.68 µs | 118.6 µs |
| 3 | after | 16.0/16.7/24.7 | 1.90 µs | 2.85 µs | 212.9 µs |

A direct call is back at the pre-H1 cost (§2: 1.63 µs) with every PL-H1
limit still in place; against §2's post-H1 figure (34.9 µs, measured
under heavier load) it is ~20×. The MCP route does not move: its ~85 µs is
the HTTP round trip and JSON-RPC handling, not the guest call.

**Trade-off.** Running in place means the *awaiting task* does not make
progress until the guest returns: a `select!` branch or a `timeout` around
`invoke_tool` in the same task waits for the call, bounded by the call's
own deadline (60 s ceiling; a plugin tool typically takes milliseconds).
Other tasks are unaffected. Before, dropping the future returned at once
and the guest ran on in the background until its deadline.

## §6 PL-H1b (AGE-706): WASI sleeps obey the deadline

Not a performance change, recorded here because it closes the last way a
guest call could outlive its deadline: a `wasi:clocks` subscription
polled through `wasi:io/poll` (`std::thread::sleep`) blocked inside WASI's
own `block_on`, out of the epoch's reach. The host's
`wasi:clocks/monotonic-clock` now caps each subscription at the call
deadline (`host::add_deadline_clock_to_linker`). With the new `sleep`
fixture, a 5 s sleep under `max_execution_ms = 500` took 5.02 s before
and ends `deadline exceeded` in under 1 s now
(`wasi_sleep_past_deadline_is_interrupted`); a 50 ms sleep inside the
budget still completes (`wasi_short_sleep_is_allowed`). A wait that ends
before the deadline takes WASI's own path, so it costs nothing extra.
