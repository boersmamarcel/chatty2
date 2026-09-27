# Fabric hop latency: baseline before BI-3

**When to read this:** You are doing BI-8 (ADR-0020's kill-criteria check)
and need the number the new path's hop latency is compared against.

Recorded by AGE-632 (BI-0) on `main` at `e432477`, before BI-3, with the swarm
test kit (`docs/swarm-test-kit.md`).

## What a hop is

From the leader's `invoke_agent` call to the broker handing the child its
`task` frame, both read off tracing timestamps in the leader's process: the
kit logs `swarm kit: invoke_agent call` just before the call, and
`LocalRunner::run_task` logs `Delegated a task to a local worker` right after
`submit_task` writes the frame. On this path a hop spawns a `chatty-tui`
worker process and waits for it to register, so process start-up dominates.

200 scripted delegations, one at a time, each answered by one `Text` reply
from the fake model.

```bash
cargo test -p chatty-tui fabric_hop_latency_baseline -- --ignored --nocapture
```

## Result

Debug build, Linux, 4 vCPUs (the cloud container this issue ran in).

| Run | p50 | p95 | mean | min | max |
|---|---|---|---|---|---|
| 1 | 463.7 ms | 523.8 ms | 469.8 ms | 423.5 ms | 566.1 ms |
| 2 | 474.1 ms | 531.9 ms | 479.4 ms | 423.2 ms | 561.7 ms |

**Baseline: p50 463.7 ms, p95 523.8 ms** (run 1). BI-8's metric is new p95 −
baseline p95, measured with `measure_hop_latency` unchanged on comparable
hardware; run 2 shows the run-to-run spread (≈ 10 ms at p50 and p95).
