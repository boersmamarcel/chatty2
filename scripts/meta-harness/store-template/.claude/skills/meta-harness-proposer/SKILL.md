---
name: meta-harness-proposer
description: Propose new chatty harness candidates for the AGE-862 Meta-Harness search on DABstep. Use when asked to run a proposer iteration in this experience store.
---

# Meta-Harness proposer (AGE-862)

You improve the *harness* around a fixed agent (chatty-tui v0.7.0, a local Qwen3.8-27B INT4
model, `--think false`) that answers DABstep data-analysis questions (payments data, a manual
of fee rules). The model, the binary and the tasks are fixed; only the candidate files change.
Each iteration you write **exactly 2 new candidates**. They are scored later, outside your
session, on a 40-task search set; you never run evaluations yourself.

## The store (your working directory)

```
candidates/<id>/harness/      the candidate's files (below)
candidates/<id>/meta.json     id, parent, source (baseline | proposer | knob), rationale, changes
candidates/<id>/scores.json   search-set results: runs.search-r<k>.tasks.<task>.{reward, tokens, error}
candidates/<id>/traces/search-r<k>/<n>.trace.txt   full transcript of each task run: every tool call
                              with its input and output, and the final answer (+ .usage.json:
                              tokens, calls, duration, exit; .atif.json is thin in v0.7.0 headless)
ruled-out.md                  what was already tried by hand and did nothing - read it first
./mh frontier | top [k] | diff <a> <b> | show <id> | status | validate <id>
```

`c000-baseline` is the current default. Its search set was run 3 times; the spread of those
three means is the noise band. A candidate only counts if it clearly beats the band.
`k-*` candidates are an automatic knob grid; `p<iter>-<n>-*` are proposer candidates.

## How to work (traces first)

1. `./mh frontier` and `./mh top 8`; read `ruled-out.md`.
2. Pick failing and flipping tasks from `scores.json` files (a task a candidate solves in one
   run and fails in another is noise, not signal). Read their traces **selectively**:
   `grep -n "\[tool:" <n>.trace.txt` gives the call sequence; then read the window you need
   (`sed -n`/`head`/`tail`) rather than a whole file (they are 20-100 KB). The task's question
   is at the top of each trace. Find *why* runs fail: wrong reading of the manual, wrong filter, formatting, a tool
   misuse, running out of time or context, giving up early, printing huge outputs.
3. Compare a better and a worse candidate with `./mh diff <a> <b>`: which tasks flipped and what
   the traces show at the point they diverge.
4. Form one hypothesis per candidate, tied to observed traces. Prefer changes that fix a
   failure mode seen on several tasks over one-off fixes. Make the two candidates explore
   different hypotheses (or one exploit + one explore).

## Writing a candidate

- Next id: `p<iteration>-<1|2>-<short-slug>` (iteration is given in the prompt). Copy a parent's
  `harness/` (any candidate, usually a strong one), then edit.
- Allowed files in `harness/`:
  - `preamble.md` (required): the system-prompt policy, appended after chatty's base prompt.
  - `BRIEF.md`: workspace conventions the preamble tells the agent to read first.
  - `helper.py`: helper code the agent may import. General to the documented data model only.
  - `skills/<name>/SKILL.md`: chatty skills (the agent can `read_skill` them; the frontmatter
    needs `name` and `description`).
  - `knobs.json`: `max_agent_turns` (int, 0 = none), `max_duration` (e.g. "20m"; the run is
    killed at about 40 min), `tool_loading` ("all" | "dynamic"), `tools` ("coder" | ...),
    `only` (list of tool groups from shell, fs-read, fs-write, git, code-exec), `think` (bool).
- Write `meta.json`: `{"id", "parent", "source": "proposer", "iteration", "rationale": one line,
  "changes": ["<file>: <what> - <why, citing trace evidence e.g. task 1744 r1 step 12>", ...]}`.
- Run `./mh validate <id>` and fix every error until it prints `valid`.

## Forbidden (a violation voids the candidate and the run)

- Task ids or question text, expected answers, any constant that only fits particular
  questions, or rules not stated in the dataset's own documentation.
- Reading or inferring grader/verifier output beyond the reward in `scores.json`; looking for
  answer files.
- Reading anything outside this directory: in particular `../tasks.json`, `../heldout/`,
  `../jobs/`, the test set and EV-7 results.
- Editing anything outside `candidates/<your new ids>/`; editing existing candidates; running
  evaluations, harbor, docker or chatty-tui.

## Finish

End with a 4-line summary per candidate: id, parent, hypothesis, evidence. Then stop.
