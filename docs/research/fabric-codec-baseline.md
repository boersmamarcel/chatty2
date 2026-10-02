# Fabric codec baseline: the v3 envelope against the v2 frames

**When to read this:** You are checking ADR-0021's third kill criterion (a
v3/v2 median ratio above 2 fails a step), or changing the worker socket's
`FrameCodec` and want the numbers it has to stay near.

Measured by AGE-769 (EN-1) on 2026-09-29, one session on one machine: an
Intel i9-10940X (28 threads), Linux 6.8, shared with other agents' builds;
every run under `nice -n 19`. The two sides ran interleaved (v2, v3, v2,
v3), release builds of the same scenario, in the same target directory.

- **v2**: `crates/chatty-protocol-gateway/benches/frame_codec.rs` on a local
  branch from `1ea4bb0b`, driving the free `encode_frame` / `decode_frame`.
  The branch was never pushed; its bench differs from the v3 one only in
  the two calls it makes.
- **v3**: the same file on EN-1's branch, driving a `BrokerCodec` and a
  `WorkerCodec` per iteration, as a connection does.

```bash
nice -n 19 cargo bench -p chatty-protocol-gateway --bench frame_codec
```

## The scenario

One task's traffic. `encode`: the broker encodes 1 `task` (v3: `task.run`)
and the worker 1,000 lines, alternating `working` statuses naming a tool and
answer chunks (v3: `task.event`s of kind `status` and `artifact`). `decode`:
the worker decodes the task and the broker the 1,000 lines. The v3 `encode`
also makes both codecs and has the worker decode the `task.run`, since a
worker cannot name the run before it has read it; the v3 `decode` starts
from a broker codec that already sent the `task.run` (set up outside the
timing), and makes the worker's codec inside it.

## Results

Criterion medians, in µs per iteration (1 task and 1,000 lines), with the
median's 95 % interval.

| Run | Load average (1/5/15 min) | encode | decode |
|---|---|---|---|
| v2, 1st | 3.01 / 7.41 / 8.57 | 546.18 [544.78, 548.29] | 708.31 [703.02, 722.14] |
| v3, 1st | 2.68 / 6.97 / 8.39 | 217.88 [216.86, 220.51] | 584.67 [583.32, 586.09] |
| v2, 2nd | 4.60 / 7.15 / 8.42 | 558.41 [557.66, 559.27] | 707.94 [707.21, 709.00] |
| v3, 2nd | 3.73 / 6.73 / 8.25 | 216.46 [216.29, 216.74] | 576.98 [576.37, 577.67] |

| | v2 median | v3 median | v3/v2 |
|---|---|---|---|
| encode (mean of the two runs) | 552.3 µs | 217.2 µs | **0.39** |
| decode (mean of the two runs) | 708.1 µs | 580.8 µs | **0.82** |

Both ratios are well under 2, so kill criterion 3 holds for step 1.

## Re-run for step 3 (EN-3a, AGE-772)

Measured on 2026-10-02 on the same machine, the same way (interleaved, `nice
-n 19`, release builds in one target directory, the v2 side again a local
branch from `1ea4bb0b`), after the payloads became typed: `task.event`
statuses carry no metadata, the terminal one a `TaskMetadata`, and every
struct denies unknown fields.

| Run | Load average (1/5/15 min) | encode | decode |
|---|---|---|---|
| v2, 1st | 5.76 / 9.31 / 9.62 | 524.19 [522.29, 526.83] | 659.09 [657.93, 660.86] |
| v3, 1st | 4.76 / 8.86 / 9.46 | 207.21 [206.68, 208.07] | 566.95 [565.27, 568.44] |
| v2, 2nd | 3.84 / 8.38 / 9.29 | 523.31 [520.79, 524.72] | 657.42 [655.31, 659.39] |
| v3, 2nd | 3.11 / 7.92 / 9.11 | 208.12 [206.90, 209.02] | 571.54 [569.19, 574.00] |

| | v2 median | v3 median | v3/v2 |
|---|---|---|---|
| encode (mean of the two runs) | 523.8 µs | 207.7 µs | **0.40** |
| decode (mean of the two runs) | 658.3 µs | 569.2 µs | **0.86** |

Both ratios are within a few hundredths of step 1's, well under 2:
kill criterion 3 holds for step 3.

## Why v3 is not slower

v2's `encode_frame` serialised each frame to a `serde_json::Value`, inserted
`v` and serialised the map again; the codec serialises one borrowed struct
straight to a string. v2's `decode_frame` parsed each line into a `Value`,
removed `v` and deserialised the rest; the codec reads the envelope with its
payload left as a `RawValue` and deserialises the params once. The id state
(a lock and a hash lookup per line) costs less than either intermediate
`Value` did.
