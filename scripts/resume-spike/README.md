# The resume spike

Measures whether giving a worker a follow-up with its own conversation
restored (arm C, *cold resume*) costs less per solved task than briefing a
fresh worker (arm R, *re-brief*), above all after the provider's prompt cache
has expired. Handles (RC-3, RC-4) are built only if arm C wins. Spec: vault
`dev/projects/fabric-resumable-conversations.md` §3. Built by RC-1 (AGE-650);
run by RC-2 (AGE-651).

## Run it

```bash
export OPENROUTER_API_KEY=...        # the OpenRouter runs only
scripts/resume-spike/run.sh --provider openrouter --model <priced model> --pairs 24 --condition cold
scripts/resume-spike/run.sh --provider openrouter --model <priced model> --pairs 24 --condition warm
scripts/resume-spike/run.sh --provider ollama --model <local model> --pairs 24 --condition cold
python3 scripts/resume-spike/report.py          # → docs/research/resume-spike-<today>.md
```

`run.sh` builds `target/release/chatty-tui` when it is missing or older than
the sources (`CHATTY_TUI=<path>` or `--chatty-tui <path>` uses another
binary). Results land in `target/resume-spike/<run-id>/`. A cold run of 24
pairs takes several hours: each follow-up first waits out the cache (6 min by
default), so budget about 24 × (12 min + three runs).

| Flag | Meaning |
| -- | -- |
| `--provider openrouter\|ollama\|openai\|fake` | OpenRouter needs `OPENROUTER_API_KEY`; Ollama defaults to `http://localhost:11434`; `openai` speaks plain OpenAI chat/completions against any `--base-url` (e.g. a local vLLM) and is unpriced unless `--prices` is given; `fake` is the dry run's (below) |
| `--model <id>` | The provider's model id, e.g. `anthropic/claude-sonnet-4.5` or `qwen3:14b` |
| `--pairs N` | Pair *i* uses task *i* of the sorted task set, wrapping around past 24 (the 50-pair re-run repeats tasks) |
| `--condition warm\|cold` | *warm*: each follow-up starts at once. *cold*: each waits `--cold-wait` seconds first |
| `--arm rebrief\|resume\|handles` | Run one arm only (default both). `handles` exits with an error until RC-3/RC-4 exist |
| `--cold-wait S` | The cold wait: the provider's cache TTL + 60 s. Default 360 (a 5-minute TTL) |
| `--base-url URL` | Another endpoint for the provider |
| `--prices IN,OUT[,CR[,CW]]` | USD per million tokens. OpenRouter's are read from its `/models` list when omitted; an Ollama run is unpriced unless given |
| `--run-id ID` | Name the run directory; re-running an existing id skips the pairs it already recorded |
| `--max-turns N`, `--max-duration D` | Each worker's budget (default 40 tool turns, `20m`) |
| `--think true\|false` | Passed to every worker (`chatty-tui --think`) |
| `--out DIR`, `--tasks DIR`, `--keep-work` | Results root, task set, keep the scratch worktrees |

**Ollama.** Set `OLLAMA_CONTEXT_LENGTH` (32768 or more) before a run: chatty
does not set `num_ctx`, and Ollama silently truncates past its default. For
the cold condition, set `OLLAMA_KEEP_ALIVE` shorter than the cold wait (the
default 5m is) so the model, and the prompt cache with it, is really unloaded
before each follow-up; each arm records whether it was still loaded
(`model_loaded_at_start`). Ollama reports no cache split, so the runner puts a
pass-through meter in front of it and records each call's prompt-eval count
and time (`prompt_eval`), the co-metric the spec asks for.

## What one pair does

1. A fresh `chatty-tui --headless --tools coder` in a new git repository
   holding the task's `repo/` gets the task (`--save-conversation` keeps its
   conversation). The runner commits whatever it left uncommitted.
2. **Arm C (resume)** runs first: in that same directory,
   `chatty-tui --headless --restore <conversation.json>` gets the frozen
   resume prompt: the commits that landed since, then the follow-up.
3. **Arm R (re-brief)**: a fresh worker in a new worktree at the same commit
   gets the frozen re-brief prompt: the task, the first worker's final answer
   as the leader's summary, its commits, then the follow-up.
4. Each arm's result is committed, diffed against the first result
   (`divergence`, and `arm_divergence` between the two arms) and judged by the
   task's `verify.py`.

Both prompts are the two blocks of
[`docs/research/resume-spike-template.md`](../../docs/research/resume-spike-template.md),
read from that file at run time; `rebrief_template_is_frozen` pins its hash.
Workers run on this machine with shell and file tools, auto-approved, in a
scratch directory under a throwaway `HOME` (which holds the provider key);
both are deleted when the run ends, so the results directory is safe to push.

## Results

`target/resume-spike/<run-id>/meta.json` (provider, model, condition,
prices, template hash, chatty2 rev) and, per pair,
`pairs/<NN-task>/pair.json` with each run's `--usage-file` object, wall-clock,
exit code, verifier result, divergence and (Ollama) prompt-eval numbers. Each
run's prompt, stdout, stderr, usage file, verifier log and diff sit beside it,
and the first run's conversation as `first.conversation.json`. A pair is valid
when its first run completed and both arms left a usage file; an arm that
errors out counts as not solved.

## The report

```bash
python3 scripts/resume-spike/report.py [RESULTS ...] [--out FILE] [--json FILE] [--min-pairs 20]
```

Reads every run under `RESULTS` (default `target/resume-spike`), pools runs
with the same provider, model and condition, and writes
`docs/research/resume-spike-<date>.md` (add it to `docs/INDEX.md` and
`docs-site/src/SUMMARY.md` before committing it): the verdict, a paired table
per group, cost per solved task per arm with the C/R ratio, wall-clock and
pass-rate deltas.

- **Cost** is computed at read time from the recorded tokens: `TokenPricing`'s
  formula for a priced model, uncached input + output tokens for an unpriced
  one. The first task is shared by both arms and left out of both.
- **Kill criterion**, per cold group (§3.3): arm C's cost per solved task at
  least 20 % lower, wall-clock at most 10 % worse, pass rate at most 5 points
  lower, over at least 20 valid pairs. A metric within 3 points of its bound
  gives `RE-RUN AT 50 PAIRS`, unless another misses by more than 3 points
  (`FAIL`). Overall: `FAIL` if any cold group fails, `PASS` only when every
  cold group passes. Warm groups are informational.

## The task set

`tasks/NN-slug/` holds `task.json` (`title`, `kind`: `review-finding` or
`requirement-change`, `task`, `follow_up`), `repo/` (the starting
repository), `verify.py` (run as `python3 verify.py <worktree>` after the
follow-up; exit 0 is a pass; it checks the follow-up's change and the
original requirement it did not change) and `solution/` (a reference final
state, never shown to a worker). The verifiers run under the same `python3`
as the runner and are written for Python 3.6+.

A new task needs all four; `resume_spike_dry_run` asserts there are at least
24. Check a verifier by hand: it must fail on `repo/` and pass on `repo/`
with `solution/` copied over it.

## The dry run

`resume_spike_dry_run` (`crates/chatty-tui/tests/resume_spike.rs`, part of
`cargo test`, so CI runs it) starts the fake model (`chatty_core::testing::fake_model`)
and runs this whole pipeline on it: `run.sh --provider fake` for three pairs
whose scripted usage passes the kill criterion, and `run.sh --provider ollama`
against a fake Ollama endpoint for three pairs whose numbers fail it, then
`report.py` on each. Real workers, real verifiers, no network.

```bash
cargo test -p chatty-tui --test resume_spike
```
