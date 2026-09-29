# Fabric hop latency: the new path against the pre-BI-3 baseline

**When to read this:** You are checking ADR-0020's latency kill criterion, or
wondering why a delegation hop got faster after BI-4.

Measured by AGE-640 (BI-8) on 2026-09-29, `main` at `3f80bd01` (after BI-3…BI-7),
with the same method as the baseline
([`fabric-hop-latency-baseline.md`](fabric-hop-latency-baseline.md)):
`measure_hop_latency(200)`, run by the ignored `fabric_hop_latency_baseline`.

```bash
nice -n 19 cargo test -p chatty-tui --bin chatty-tui fabric_hop_latency_baseline -- --ignored --nocapture
```

## Same box, both sides

The recorded baseline (p95 523.8 ms) came from a 4-vCPU cloud container. The
bound is a delta, so the baseline was measured again on the box that measured
the new path: `main` at `d39ffa0f`, the parent of BI-3's merge (`eba16104`).
Debug builds, Linux, 28 cores, shared with other builds; each run under
`nice -n 19`, with the 1-minute load average printed before it.

| Tree | Run | load (1 min) | p50 | p95 | mean | min | max |
|---|---|---|---|---|---|---|---|
| before BI-3 (`d39ffa0f`) | 1 | 5.68 | 428.1 ms | 446.7 ms | 428.9 ms | 406.1 ms | 452.7 ms |
| before BI-3 | 2 | 2.53 | 425.5 ms | 446.7 ms | 427.5 ms | 408.2 ms | 467.1 ms |
| before BI-3 | 3 | 1.79 | 424.2 ms | 442.3 ms | 425.4 ms | 404.6 ms | 455.1 ms |
| before BI-3 | 4 | 1.67 | 424.6 ms | 443.0 ms | 425.8 ms | 408.2 ms | 462.6 ms |
| before BI-3 | 5 | 1.43 | 425.7 ms | 441.4 ms | 426.3 ms | 406.6 ms | 449.2 ms |
| new path (`3f80bd01`) | 1 | 3.69 | 53.1 ms | 61.1 ms | 54.3 ms | 47.2 ms | 109.0 ms |
| new path | 2 | 3.00 | 52.7 ms | 61.6 ms | 53.8 ms | 47.6 ms | 65.6 ms |
| new path | 3 | 2.18 | 52.1 ms | 61.8 ms | 53.8 ms | 48.0 ms | 72.3 ms |
| new path | 4 | 1.73 | 52.5 ms | 61.4 ms | 53.9 ms | 47.7 ms | 65.5 ms |
| new path | 5 | 1.79 | 52.5 ms | 60.6 ms | 53.6 ms | 47.5 ms | 64.8 ms |

**New p95 − baseline p95 = 61.8 − 441.4 = −379.6 ms** (worst new run against
the best baseline run). Against the recorded cloud baseline it is
61.8 − 523.8 = −462.0 ms. The bound is ≤ +50 ms; it holds under load and
at the lowest load seen alike.

## Why the hop shrank, and what did not change

The hop ends when `LocalRunner::run_task` has written the `task` frame, which
waits for the worker to register. Before BI-3 a worker built its whole
agent (config, providers, tools) before it registered. Since BI-4 a worker
connects, gets `welcome`, and builds *after* it has its task
(connect-then-build), so that build time left the hop.

So the hop metric improved without the delegation getting faster end to
end. Timing `run_leader` around each of the same 200 calls (an uncommitted
instrumentation of `measure_hop_latency`, runs 4 and 5 above):

| Tree | Run | end-to-end p50 | end-to-end p95 |
|---|---|---|---|
| before BI-3 | 4 | 451.3 ms | 469.1 ms |
| before BI-3 | 5 | 452.8 ms | 469.3 ms |
| new path | 4 | 444.5 ms | 464.1 ms |
| new path | 5 | 445.9 ms | 466.3 ms |

End to end, a one-reply delegation is about 5 ms faster at p95 on the new path:
the root broker's extra hop costs nothing measurable here, and neither side
has added a regression that the hop metric hides.
