# Swarm test kit

**When to read this:** You are writing a test for anything that spans
processes — a leader, its broker, real `chatty-tui` workers, a sub-leader's
own broker — and it must run in CI with no network and no real model
(AGE-632, the fabric epics' BI-0).

Two pieces:

- **`chatty_core::testing::fake_model`** (behind `chatty-core/test-support`):
  `FakeDaemon`, a localhost model server. `FakeDaemon::scripted(Script)`
  answers from a script; `FakeDaemon::ollama`/`openai_compatible` replay
  canned bodies in order (what `session::append_only_prefix` uses).
- **`crates/chatty-tui/src/participant/swarm_kit.rs`** (test-only):
  `SwarmKit`, a real root `Broker` whose roster's models point at two fake
  servers, spawning real `chatty-tui` workers.

## Scripting the model

A `Script` maps a routing key to a queue of `Reply`s. A key matches a request
whose `model` field equals it or whose system prompt contains it; the first
matching route wins.

| Reply | Effect |
|---|---|
| `Text(s)` | Ends the provider turn with `s` |
| `ToolCalls(vec![(name, args)])` / `Reply::tool_call(name, args)` | Answers with those tool calls (ids `call_0`, `call_1`, … per server) |
| `Usage { input, output, cache_read }` | The usage the next answer reports (default 10/5/0) |
| `Delay(ms)` | Sleep, then serve the next reply |
| `Error(status)` | Answer with that HTTP status |

Replies are OpenAI-compatible SSE on `…/chat/completions` and Ollama NDJSON on
`/api/chat`. A request no route matches, or whose queue ran dry, gets HTTP 500
naming its model — a missing script fails loudly rather than hanging.

Every request is recorded (`requests()`, `requests_for(key)`) with its body,
arrival and finish time; `max_concurrency()` is the peak number in flight.
One server is one endpoint.

Mind the headless recovery policy when scripting errors: a worker retries any
provider status other than 401/403/429 after 10 s, 20 s, …, and a 401 once
immediately. Two `Error(401)`s end a worker's turn fast.

## Writing a swarm test

```rust
use super::swarm_kit::{AgentDef, Endpoint, SwarmKit};
use chatty_core::testing::fake_model::{Reply, Script};

#[tokio::test]
async fn my_swarm_feature() {
    let kit = SwarmKit::start(
        vec![AgentDef::new("kit-worker", "kit/worker", Endpoint::Sse)],
        Script::new().route("kit/worker", [
            Reply::tool_call("read_file", serde_json::json!({ "path": "README.md" })),
            Reply::text("done"),
        ]),
        Script::new(), // the NDJSON endpoint's script
    )
    .await;

    let run = kit.run_leader("read the readme").await;
    // run.output: Result<InvokeAgentOutput, String>, what the leader's model sees
    // run.progress: every InvokeAgentProgress, in order
    // run.requests / kit.sse.requests_for("kit/worker"): the model's view
}
```

- Each agent's **model identifier is its routing key**. `Endpoint::Sse` is an
  OpenRouter-type provider, `Endpoint::Ndjson` an Ollama one: a provider is
  looked up by type, so these are the two separate endpoints a roster can
  have.
- `AgentDef::sub_leader()` starts that worker with `--broker`; it serves the
  same roster (the kit's `module_settings.json`), so it can delegate to its
  siblings.
- Workers run auto-approved, with filesystem tools on, fetch and memory off,
  in a non-git workspace holding `README.md` (`# Chatty`).
- A worker is `worker_executable()`'s `chatty-tui` behind a two-line `sh`
  wrapper that only sets `HOME` and the XDG dirs to the kit's temp dir, so it
  never reads your real settings. `cargo test -p chatty-tui` builds the
  binary (`tests/worker_binary.rs` is what makes it); `CHATTY_WORKER_EXE`
  overrides which binary `worker_executable()` picks.
- `normalize` / `parent_trace` strip the temp dir, ports and uuids; request
  bodies are otherwise byte-stable run to run (`swarm_kit_is_deterministic`).

## Pre-change goldens

`src/participant/goldens/pre_fabric/` holds the parent-side traces of
`equivalence.rs`'s scenarios and nested delegation on separate endpoints,
recorded through the kit on `main` before BI-3 (invariant 10 of the
broker-identity spec). `pre_fabric_goldens_replay` replays them.
`UPDATE_GOLDENS=1` does not touch them, and `assert_pre_fabric` never
overwrites an existing file; a later PR may only delete one, with a reason in
its PR body. CI's `fabric-goldens` job runs the replay on its own and fails a
pull request that modifies an existing golden (ADR-0020's first kill
criterion, AGE-640).

The hop-latency baseline (`measure_hop_latency`, run by the ignored
`fabric_hop_latency_baseline`) is in
[`research/fabric-hop-latency-baseline.md`](research/fabric-hop-latency-baseline.md);
the new path, measured the same way, is in
[`research/fabric-hop-latency-2026-09-29.md`](research/fabric-hop-latency-2026-09-29.md).
