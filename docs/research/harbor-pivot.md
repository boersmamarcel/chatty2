# Harbor pivot — Stage B eval

**Date:** 2026-08-26

Stage B coding / environment benchmarks do not live in chatty2. There is **no `chatty-eval` crate**.

| Concern | Location |
| -- | -- |
| Containerized Stage B (HumanEval, Polyglot, AppWorld, ALFWorld) | [`harbor-chatty`](../../../harbor-chatty) — Linear AGE-34 |
| Paired stats, MDE, ablation flags, QA loaders for optimizers | `chatty-optimize` |
| `FeedbackFn` / ATIF round-trip | `chatty-trace` (AGE-5, after walking skeleton) |

Harbor: <https://github.com/harbor-framework/harbor>
