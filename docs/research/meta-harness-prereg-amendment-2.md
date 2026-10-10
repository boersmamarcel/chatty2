# AGE-862 Meta-Harness: prereg amendment 2 (binary v0.7.0 → v0.7.1, full ATIF traces)

Amends `meta-harness-prereg.md` (2aba014b, sha256 `2cd1c6b7…`) and amendment 1 (2dbb5df5).
Decided by Marcel on 2026-10-10 and committed before any candidate was scored. Only baseline
search r1 had finished or was running; no candidate, test-set or EV-7 run had started.

## Change

- **Binary:** chatty-tui **v0.7.1** (the commit tagged `v0.7.1`) replaces v0.7.0 (`d2d35264`) for
  every run that starts after its tarball is built and verified. That covers the remaining baseline
  rounds, all candidates, the test set and EV-7. The sha is written to `TARBALL_SHA`, and each
  run records the binary it used in `scores.json` (`runs.*.binary`).
- **Why:** v0.7.0's headless `--export-atif` writes only the user message and the final text, with
  no tool calls (AGE-863). The paper's main ablation says the proposer needs the raw traces.
  chatty2 #1103 fixes the export.
- **Scope of #1103:** it touches only `crates/chatty-core/src/exporters/atif_exporter/{mod,swarm,tests}.rs`
  and `crates/chatty-tui/tests/headless_export.rs`. It changes no runtime behaviour: prompts,
  tools, the agent loop and model requests are unchanged. That is why baseline runs on v0.7.0 and
  v0.7.1 are pooled in one noise band.
- **Completed runs:** baseline runs finished on v0.7.0 keep their scores. Their traces stay as the
  stdout transcript `trace.txt`, beside a thin `atif.json`.
- **Proposer:** it reads `atif.json` where the run has a full export (v0.7.1) and `trace.txt`
  otherwise.
- **Before arming:** the v0.7.1 tarball is checked on one real headless DABstep task, and its
  `atif.json` must contain the tool calls.
- **Candidates are held** (the queue's `HOLD_CANDIDATES`) until that check passes. Baseline rounds
  keep the GPU busy meanwhile.

The gates (§6) are unchanged.
