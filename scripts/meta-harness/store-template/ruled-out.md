# Ruled out (or not worth repeating) before this search

Hand-run harness work on the same local model (Qwen3.8-27B INT4, vLLM), 2026-09-23 to 09-28,
register E0-E9 (vault `dev/research/chatty-harness-experiments.md`). Stopped by Marcel after
48 h with no real gain. Lesson: once the harness reached parity (v0.4.0), single prompt-line
tweaks had low priors, and single runs on 10-50 tasks swing by +-2 to 6 tasks. Do not spend a
candidate on something below unless your traces show a specific, different reason it would work.

| What was tried | Result |
|---|---|
| **E5a "rules as code" prompt line** (DABstep-50): "write the documentation's matching rules (incl. what an empty field means) as one small function, check it on one hand-verifiable case, then compute" | FAILED: 17/50 vs 19/50, cost +25 %. The AGE-9 74 % came from domain knowledge handed over in `helper.py`, not from a habit a generic prompt line installs. |
| **E2 "compute, don't reason" + scratch-writes-not-progress + read_file path suggestions** (GAIA 16 x2, SWE) | FAILED: GAIA 17/32 vs 19/32. |
| **E2b the two tool fixes alone** | FAILED: 13/22 vs 18/22. |
| **Stop rule / explicit work order in the prompt** (bundle 4) | No evidence it helps (SWE p = 0.51), weak evidence it hurts: the model stopped on its own self-check. |
| **Search page extracts** (bundle 5) | GAIA 5/16 vs 11/16; a look-alike fact in an extract caused a loss. Not relevant to DABstep (no web). |
| **E1 announce-and-stop nudge** (continue after a text-only "Let me ..." turn) | Shipped as a bug fix in the binary already; rare (12/160 runs). Do not re-add as prompt text. |
| **E3 same-model judge over k=2 attempts** (GAIA-80) | Inconclusive (+5/80, p = 0.18). On DABstep (AGE-9) a judge did help, but that is test-time compute (k runs), not a single-run harness change; out of scope here. |
| **Thinking mode on** (AGE-9, DABstep) | Hurt accuracy and cost much more time. Only the `think` knob in the knob grid tests it again. |
| **Todo/plan tool over-use** (AGE-479) | Already gated in the binary; prompting the model to plan more did not help multi-step tasks on this model. |
| **Crosscheck: Data team (best-of 3 + adjudicator)** (AGE-853) | Expensive (~4x the single run's time); not a single-run harness change. Out of scope. |

What is known to matter on DABstep with this model:
- Domain conventions handed over as code (`helper.py` v4: fee-rule matching, wildcard semantics,
  month/day windows) and a `BRIEF.md` that tells the agent to use them: the big gain (AGE-9, AGE-754).
- Answer formatting: the question's example format is a shape, not text to copy (placeholders,
  ellipses, trailing commas were copied before the preamble said so).
- Context: printing whole files or large tables fills the 32k-ish window and degrades later turns.

Allowed, but note the trap: any change to `helper.py` or `BRIEF.md` must stay general to the
documented data model (what the manual says), never a constant or a rule that only fits one
question. Task ids, expected answers and grader output are off limits (see the skill).
