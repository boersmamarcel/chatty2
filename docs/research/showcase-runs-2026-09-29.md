# Showcase teams: real-model reliability runs (2026-09-29)

**When to read this:** You want to know whether the shipped example teams
(`data-analysis`, `research-brief`, and the coding team `fix-and-verify`,
AGE-757) work on a real model, and how reliably, or why `coder-reviewer` and
the Benford team were not shipped (AGE-752).

## Setup

- Desktop app (`chatty`, this branch) under Xvfb, a scratch config and data
  directory, gateway port 8531, approval mode **Always ask**. Every run is
  `/agent <leader> …` in a new chat. That is the tutorial's own path.
- Model: `Qwen3.8-27B-INT4` on a local vLLM (one RTX 3090, 32k context,
  `extra_params.think = "false"`).
- Runs were serialized: one at a time on the one model server.
- Approvals: every relayed approval card was answered **Approve**, in S4 by
  a click and otherwise by a script that presses Alt+Y in the app for each
  approval the desktop logs.
- Metrics per run: wall time (conversation created → last update), agents
  spawned (the broker's `Delegated a task` log lines), approvals (the relayed
  approval log lines), and tokens per model (the conversation's usage lines).
- *Success* means the final answer is correct and complete against the
  fixture's known answer, and it states nothing false as fact.

## Results (what ships)

### `data-analysis`: `/agent data-lead Revenue in orders.csv fell in August. Find out why.`

The known answer (`teams/data-analysis/fixture/orders.csv`): August revenue is
43,886.15 against 53,251.00 (−17.6%). `Pro` orders fall from 132 to 91 at an
unchanged price, most of them in the EU: EU `Pro` alone is −6,596.85 (about
70%), 47 orders down to 14. The online `SUMMER30` code (30% off `Starter` from
1 August) adds 1,764 of discount with no volume lift. A good report also says
the file shows *how much*, not *why*.

The shipped analyst computes with `query_data` (SQL) and has no shell. The
lead has it save the reviewed report as `report.md`, which is the one write
that asks under **Always ask**.

| Run | Success | Wall | Agents | Approvals | Tokens (in + out) |
|---|---|---|---|---|---|
| S1 | yes: −17.6%, Pro 86.6% and EU 74.2% of the drop, SUMMER30 cost 1,764, "the data does not show why" | 11m25s | 7 | 2 (`report.md`, plus a stray `answer.txt` from `final_answer`) | 459,483 + 19,488 |
| S2 | yes: EU × Pro −6,596.85 (70.4%), 47 → 14 orders at an unchanged price, SUMMER30 | 10m04s | 4 | 1 | 296,598 + 12,962 |
| S3 | yes: EU and Pro as the concentration, SUMMER30 as the discount driver; EU × Pro not quantified as one cell | 16m34s | 6 | 1 | 569,177 + 16,662 |
| S4 (the tutorial's screenshot run) | yes: Pro −31% of units and 86.6% of the drop, EU 74%, SUMMER30 1,411 of extra discount, cause left open | 15m53s | 4 | 1 | 299,204 + 12,651 |

4 of 4. The spread in wall time comes from the review loop: the reviewer
re-derives the numbers and often asks for one round of corrections.

### `research-brief`: `/agent editor Using the documents in docs/, write brief.md: due diligence on Globex Corp. …`

The known answer (`teams/research-brief/fixture/docs/`): Globex Corp is a
consulting vendor with no contract and no statement of work. Policy note PN-7
requires a signed statement of work before the first invoice. The audit
committee wants such a vendor flagged to the controller on its first invoice.
The approval limits and the 30-day no-splitting rule apply.

| Run | Success | Wall | Agents | Approvals | Tokens (in + out) |
|---|---|---|---|---|---|
| B1 | yes: all four facts, each cited to its file | 2m53s | 4 | 1 (the write) | 175,955 + 4,141 |
| B2 | yes (the brief ran to 644 words against the writer's 400-word limit) | 3m25s | 4 | 2 | 196,643 + 6,088 |
| B3 | yes: all four facts, six cited actions for finance | 3m05s | 4 | 1 | 195,156 + 5,255 |

3 of 3, and the reviewer approved on the first pass every time. The writer's
400-word limit is soft (see the tuning history).

### The tutorial's own two-agent team (`memo-lead` → `memo-writer`)

One run on the step-6 findings: 1m19s, 2 agents, 1 approval (`memo.md`),
39,518 + 1,080 tokens. The memo gave the answer first and one owned action
per finding.

## Tuning history (honest record)

- **`data-analysis`, Python analyst** (before the SQL design): 3 of 3
  succeeded after two tuning rounds (11m05s, 10m22s, 8m30s). Each run asked
  for 4 to 7 approvals, because every script and every fix of a script was a
  command. That is too many for a first demo. The SQL analyst above replaced
  it and asks once. Before that, the first run (T1) claimed the promo caused
  the EU `Pro` drop through cannibalization, which is false. A round that
  asked for "the first day each change started" produced two runs (T2, T3)
  with a wrong start date for EU `Pro` (they said 1 August; it is
  11 August). Round 2 removed the timing request, told the lead to say when
  the data shows how much but not why, and raised the reviewer's turn budget
  from 8 to 12.
- **`research-brief` word limit**: a round that had the reviewer enforce the
  400-word limit made it worse. Over 3 runs, only 1 brief was under 400
  words. The rewrites raised approvals to 4–6 and wall time to 5–10 minutes.
  That round was reverted; the shipped prompts are the ones behind B1–B3.
- **Mixed models** (`reviewer` on `qwen3:4b`, CPU): left out of this
  release's runs. The tutorial documents the one-line change without claiming
  numbers.
- **Benford `analyst-swarm`** (coordinator, data-coder, benford-analyst,
  reviewer on a 101-row ledger with a planted HIGH Benford verdict): 3 of 3
  after tuning (9m16s, 6m25s, 6m08s). It was dropped on product grounds: a
  Benford check is a narrow showcase. The `benford` plugin stays as the
  plugin-authoring example (the developer tutorial writes an `auditor` spec)
  and as a test fixture.
- **`coder-reviewer`** (headless `--team coder-reviewer --auto-approve` on a
  fresh copy of the tutorial's bank repository): 2 of 2 runs failed, and the
  third was not run. Inside its worktree, the coder's `.git` pointed at a
  gitdir its sandboxed shell could not reach. It reported "no repository",
  re-ran `git init` in place, and worked off the branch Chatty commits and
  hands to the reviewer. The reviewer then found an unchanged tree, three
  times, and the leader stopped after the skill's two retries. The team is
  not shipped. It stays as a test fixture for the pre-spec goldens and the
  team-mechanics tests.

## What the runs showed besides success

- A relayed approval from a `/agent` turn never reached the desktop card: the
  worker waited out its timeout. `delegation_stream` dropped the turn's
  approval channels. This is fixed in the same PR, with
  `slash_agent_relays_a_worker_approval`.
- The finished `/agent` turn reads "Worked for a moment" whatever its length.
- A worker that runs out of turns shows "—" as its spend in the swarm tree.
- The `coder` profile offers `final_answer`, which writes `answer.txt` into the
  workspace. S1's analyst used it once, which left a stray file and made one
  extra approval.
- The shared participant socket (`/run/user/1000/chatty/participants.sock`)
  is taken when a second chatty runs on the same machine. The broker warns
  and carries on.

## `fix-and-verify` (AGE-757): `The project's tests fail. Find the bug and fix it without changing the tests.`

The coding team that replaces `coder-reviewer`, after AGE-757 made a
worker's linked worktree reachable from its sandboxed shell. Lead `fix-lead`
(`coordinator`), `fix-coder` (`coder`), `code-reviewer` (`reviewer`, no
shell). The team's `verification` is the fixture's
`python3 -m unittest discover -s tests -t . -v`, run by Chatty in the
coder's tree; the reviewer judges the diff and that evidence.

The known answer (`teams/fix-and-verify/fixture/`): one test fails because
an order of exactly the free-shipping threshold (50.00) is charged shipping.
The fix is `>` → `>=` in `invoice.py`, with `tests/test_invoice.py`
unchanged. *Success* means the fix is merged, the four tests pass on the
merged tree, and the test file is untouched.

Setup: headless `chatty-tui --team fix-and-verify --auto-approve` on a fresh
`git init` of the fixture, same model and server as above
(`Qwen3.8-27B-INT4`, 32k, `think = "false"`), endpoint budget 1 (one model
request at a time), runs serialized. Wall is the whole run; agents counts
delegations. D1 is the desktop run behind the tutorial's screenshot
(`/agent fix-lead …`, gateway port 8548, the verification set in
`module_settings.json`).

| Run | Prompts | Success | Wall | Agents | Notes |
|---|---|---|---|---|---|
| F1 | first cut | yes | 3m52s | 2 | Approved first pass. The coder also changed `/ 100` to `/ 100.0` (a no-op, flagged by the reviewer) and ran `python` (2.7), whose `.pyc` files got committed. |
| F2 | first cut | yes | 2m29s | 2 | Approved first pass, one-character fix. |
| F3 | first cut | **no** | 8m03s | 4 | The lead told the coder to work on a branch it named; the coder switched to it with `git_switch_branch`, so Chatty's commit went there and the evidence for the coder's own branch showed no commits. Round 2 fixed it (exit 0), but the lead then looked for the commit in its own `git log`, did not find it, and the reviewer called the evidence fabricated. No merge. |
| F4 | tuned | yes | 3m24s | 2 | Approved first pass. |
| F5 | tuned | yes | 3m46s | 2 | Approved first pass. |
| F6 | tuned | yes | 1m44s | 2 | Approved first pass. |
| D1 | tuned, desktop | yes | about 3m | 3 | 229K tokens (lead 79.9K, coder 115.5K, reviewer 33.6K). The lead merged into its own branch `sub-agent/fix-lead-0`, as a desktop lead does. |

Tuned prompts (what ships): 3 of 3 headless, plus the desktop run. First
cut: 2 of 3. The tuning after F3 told the lead that each worker already has
its own branch, which lives in the shared repository and not in the lead's
tree, and to judge by the evidence block and the verdict; it told the coder
never to create or switch branches, and the reviewer that its own worktree
shows the code before the change, so it reads the change only through
`git_diff <base>..<branch>`. The fixture gained a `.gitignore` for `*.pyc`.
Tokens for the headless runs were not recorded: headless prints no usage,
and the vLLM counters were shared with another run.

Found on the way: the desktop's `/agent <leader>` takes the verification
command from `module_settings.json`, not from the team's `team.json`, so a
preset team's test command applies only under `--team`.
