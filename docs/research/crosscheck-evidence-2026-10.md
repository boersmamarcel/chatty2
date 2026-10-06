# Crosscheck: the evidence behind it (October 2026)

**When to read this:** you want to know why Chatty ships Crosscheck, a team that makes several independent attempts and lets a judge pick one, and what it buys you at what cost.

## The claim

On the same model and the same tasks, independent attempts plus a judge finish more tasks correctly than a single attempt. In our tests the gain was **+8 to +22 points**, at **2–3.3× the tokens** of a single run.

Crosscheck is opt-in per conversation and shows its cost before it runs. It is a general reliability feature: nothing in it is tuned to one benchmark.

## The comparisons

All runs use the local `RedHatAI/Qwen3.8-27B-INT4` on vLLM, with local grading against the benchmark's answers. In every row, both arms share the same harness, tools and data. The *gain* is what this note claims. Absolute scores depend on the harness (see *Caveats*).

| # | Date | Task set | Single attempt | Attempts + judge | Gain | Cost |
|---|---|---|---|---|---|---|
| 1 | 2026-09-18 | DABstep Hard, **378 tasks** (full set) | 77.2 % (292/378) | **84.9 %** (321/378), 2 attempts + judge | **+7.7** | ~2× |
| 2 | 2026-09-17 | DABstep, 80-task subset | 79.6 % (pass@1, mean of 240 attempts) | **88.75 %**, 3 attempts + judge | **+9.2** | ~3× |
| 3 | 2026-10-05 | DABstep mixed set (18 tasks where attempts disagree), general Crosscheck team, **judge broken** (see below) | 4/18 | **7/18** | **+16.7** | 2.10× |
| 4 | 2026-10-06 | Same mixed set, Crosscheck: Data after the #1084 fix, first trials of the confirmatory run (smoke test) | 5/9 | **7/9** | **+22** (small n) | ≈3.3× |

**The judge earns its place over a simple vote.** On row 2's 18 mixed tasks, a judge that reads the attempts' traces picked a correct answer 16 times. Majority vote did so 9 times (tie goes to the first sample). Over the whole 80-task subset, majority vote scored 80.0 %, level with a single attempt (79.6 %); the judge scored 88.75 %.

Sources, in the vault: `dev/research/dabstep-reflection-gap.md` (rows 1–2, the vote comparison), `swarm-results/age-853/summary.json` (row 3) and `swarm-results/age-853-c2/` (row 4).

## Why it works, and why "teams of agents" in general don't

Our own swarm benchmarks (EV-4, EV-7) found that agents **coordinating** on one task lose to a single agent. EV-7 measured −29 points at 1.38× the tokens. That matches the literature on sequential, tool-heavy work (Kim et al., arXiv 2512.08296).

Crosscheck does not coordinate. Its attempts are **independent**, in separate contexts. Its compute buys *diversity*, and the judge turns diversity into accuracy. This is the compute-for-accuracy trade with the best support in the literature: Large Language Monkeys; *More Agents Is All You Need*.

## Caveats

- **Rows 1–2 used DABstep-specific helpers in both arms.** That inflates the absolute scores (77–89 %), not the gain. The general team without those helpers scores far lower in absolute terms (row 3's single arm: 22 % on the mixed set). Absolute DABstep numbers are therefore not a claim about Crosscheck.
- **Row 3 understated Crosscheck.** A bug fixed in #1084 let the first attempt that wrote the answer file cut its siblings and the judge short: 31 of 39 picks fell back to a vote. Even so, Crosscheck gained 16.7 points.
- **Row 3's pre-registered gate failed on its no-harm set by one task** (4/30 against 5/30, bound −2 points). With 30 tasks, one flip is 3.3 points. Crosscheck shipped as opt-in on the consistent direction across rows 1–3. Marcel decided this on 2026-10-05 (#1081).
- **Small samples:** rows 3 and 4 have 18 and 9 tasks. Rows 1–2 carry the weight.
- **One model, one domain so far.** A generality check on GAIA, which is web research, runs next: 40 tasks, single attempt against Crosscheck, with a pre-registration in `swarm-results/age-853-gaia/prereg.md`. This note will be updated with its result.

## What to say about it

"In our tests, the share of tasks finished correctly rose by 8–22 points with Crosscheck, at 2–3× the tokens of a single run." Name the model and benchmarks in a footnote. Do not say "multi-agent is smarter", and do not lead with a benchmark score.
