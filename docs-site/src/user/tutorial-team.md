# Tutorial: a small agentic team

**When to read this:** You have run [one named worker](./tutorial-named-worker.md) and want a team that can take a task from "fix this" to a merged, verified branch: a leader that only delegates, a coder, and a reviewer that does not take the coder's word for it. About an hour, most of it watching.

## What you will end up with

A team directory of your own — `team.json` plus a `SKILL.md` playbook — checked into a project, run with one command, that fixes the bank account bug from the first tutorial, has the fix reviewed against acceptance criteria, merges it, and proves it with the test suite. Then you will change the roster and see the difference.

## Prerequisites

- Everything from [the first tutorial](./tutorial-named-worker.md): Ollama with `qwen3:14b` and `qwen2.5-coder:14b`, `chatty-tui`, and the `~/bank` project with its one passing test and unfixed `withdraw`.
- A clean tree in `~/bank` (`git status` shows nothing to commit).

## How a team run works

Four things happen that a single worker never does:

1. The **leader has a role too** — `coordinator`, which can read, delegate and merge but cannot edit or run commands. It has to work through its team.
2. The **coder gets its own branch** (`sub-agent/local-coder-0`; the number counts that worker's delegations in the session) in its own copy of the tree. When it finishes, Chatty commits that branch and appends an `evidence` block to its report: branch, commit count, diff summary.
3. The **verification command is run by Chatty, not by a worker**, in the worker's tree, and its exit code and last lines go into the same evidence block. A coder that says "tests pass" is checked, not trusted.
4. The **reviewer reads the branch itself** — the diff, the tests — and its first line is a verdict. The leader merges only on `APPROVE`.

The playbook that sequences this is a skill the leader is told to follow on its first turn.

## Steps

### 1. Run the team that ships with Chatty

One team is built in, `coder-reviewer`. Run it before writing your own, so you know what a good run looks like:

```bash
cd ~/bank
chatty-tui --team coder-reviewer --headless --auto-approve \
  --ollama --model qwen3:14b \
  -m "Fix Account.withdraw in bank.py. Acceptance criteria: 1. withdraw raises ValueError when the amount is not positive. 2. withdraw raises ValueError when the amount exceeds the balance. 3. Otherwise withdraw subtracts the amount and returns the new balance. 4. tests/test_bank.py gains a test for each of criteria 1-3. 5. test_deposit keeps passing. Verification: python3 -m unittest discover -s tests -t . -v. Never edit files yourself."
```

`--team` implies `--broker`. The preset's coder and reviewer name no model of their own, so they run your default model — and with `--ollama` there is no marked default, so that is whichever model Ollama lists first. The leader's `list_agents` call (on stderr) shows what each worker got: `Model: qwen2.5-coder:14b (the default)`, say. If it picked something you would not want writing code, skip ahead to step 3, where your own team names a model per worker. The task states the acceptance criteria and the verification command explicitly because that is what the playbook expects — vague tasks make vague teams.

`--headless` prints only the leader's final answer on stdout; the hand-offs go to stderr as they happen. To watch both, drop `--headless` and `-m` and type the task into the interactive app instead.

### 2. Read the run

On stderr you will see the shape of the playbook:

```
[tool: read_skill] ✓ completed
[tool: write_todos] ✓ completed
⟳ invoke_agent
[agent: local-coder] [local] ⟳ running
  …the coder's own tool calls stream here…
[agent: local-coder] ✓ completed
⟳ invoke_agent
[agent: local-reviewer] [local] ⟳ running
[agent: local-reviewer] ✓ completed
[tool: git_merge] ✓ completed
⟳ invoke_agent
[agent: local-reviewer] [local] ⟳ running
[agent: local-reviewer] ✓ completed
```

The coder's report ends with the evidence block Chatty appended:

````
```evidence
branch: sub-agent/local-coder-0
base: main
commits: 1
diff --stat main..sub-agent/local-coder-0:
 bank.py            |  6 +++++-
 tests/test_bank.py | 15 +++++++++++++++
 2 files changed, 20 insertions(+), 1 deletion(-)
verification: python3 -m unittest discover -s tests -t . -v — exit code 0
test_deposit (tests.test_bank.TestBank.test_deposit) ... ok
test_withdraw_negative (tests.test_bank.TestBank.test_withdraw_negative) ... ok
test_withdraw_overdraft (tests.test_bank.TestBank.test_withdraw_overdraft) ... ok
test_withdraw_ok (tests.test_bank.TestBank.test_withdraw_ok) ... ok
----------------------------------------------------------------------
Ran 4 tests in 0.001s

OK
```
````

The reviewer's report starts with `APPROVE` (or `REQUEST_CHANGES` and a numbered list, in which case the leader sends a fresh coder back to the branch — at most twice). The leader's final answer on stdout gives the verdict, the merge commit and a criteria-to-evidence table.

Then look at the tree:

```bash
git log --oneline --graph | head -5     # a merge commit bringing in sub-agent/local-coder-0
git branch                              # main, sub-agent/local-coder-0, sub-agent/local-reviewer-0, -1
python3 -m unittest discover -s tests -t . -v   # four tests, OK
ls .chatty/worktrees/                   # local-coder-0  local-reviewer-0  local-reviewer-1
```

Every delegation is a fresh worker on a fresh branch in its own copy of the tree — the reviewer's two (the review, then the verification pass) as much as the coder's — and only the coder's has commits. If the reviewer sent the coder back once, you will also see `local-coder-1`, told which branch to continue from. Nothing is cleaned up behind your back; the worktrees and branches stay until you remove them.

### 3. Make it your own team

A team directory is three things in one folder: the roster, the leader's role, and the playbook. First put the project back to its unfixed state, so your team gets the same job the preset had:

```bash
git checkout -q main && git reset -q --hard "$(git log --format=%H --grep='Bank account with a withdraw bug' -n 1)"
for w in .chatty/worktrees/*; do git worktree remove --force "$w"; done   # worktrees first…
git branch --list 'sub-agent/*' | xargs -r git branch -D                    # …then their branches
python3 -m unittest discover -s tests -t . && echo "back to one test"
```

Then put a team directory in the project, where it will be found ahead of the built-in preset:

```bash
mkdir -p .chatty/teams/bank-fix
cat > .chatty/teams/bank-fix/team.json <<'EOF'
{
  "leader": {
    "profile": "coordinator",
    "preamble": "You lead a two-worker team and edit nothing yourself. Restate the task as acceptance criteria, delegate the change to local-coder, have local-reviewer check the coder's branch, and merge only on APPROVE."
  },
  "agents": [
    {
      "name": "local-coder",
      "model": "qwen2.5-coder:14b",
      "tools": "coder",
      "preamble": "You are the coder. Work only in your own workspace, write a test for each acceptance criterion before making it pass, run the tests, do not commit, and report the files you changed with the test output.",
      "max_agent_turns": 30
    },
    {
      "name": "local-reviewer",
      "model": "qwen2.5-coder:14b",
      "tools": "reviewer",
      "preamble": "You are the reviewer. Read the branch's diff yourself, run the tests yourself, never edit the tree. The first line of your answer is APPROVE, REQUEST_CHANGES or BLOCKED."
    }
  ],
  "verification": "python3 -m unittest discover -s tests -t . -v",
  "skill": "bank-fix",
  "max_agent_turns": 50
}
EOF
```

What changed against the preset, and why:

| Field | Here | Why |
|-------|------|-----|
| `agents[].model` | `qwen2.5-coder:14b` on both workers | A coding model for the code; the leader keeps `qwen3:14b` for planning. Each worker is metered on its own model's server, so a worker on another machine would not queue behind the leader. |
| `agents[0].max_agent_turns` | `30` | A coder that writes three tests and runs them needs more than the default ten tool rounds. This is the coder's own budget. |
| `verification` | in the file | So the task no longer has to say it. Chatty runs this in each worker's tree at the end and puts the result in the evidence block. |
| `max_agent_turns` (top level) | `50` | The **leader's** budget for the run. A delegating leader burns a turn per hand-off and per tool call; the default ten is not enough. |
| `skill` | `bank-fix` | The playbook, next: the leader's first turn opens with *read_skill bank-fix and follow it*. |

Now the playbook. This is the part worth iterating on — it is where a team's behaviour actually lives:

```bash
cat > .chatty/teams/bank-fix/SKILL.md <<'EOF'
# Skill: bank-fix

Deliver one bounded change with a coder and a reviewer.

1. Restate the task as a numbered list of acceptance criteria and record them with the todo tool.
2. Delegate to local-coder with one self-contained prompt: the criteria verbatim, and "work only in your own workspace, write a test for each criterion before making it pass, run the tests, do not commit, and report the files you changed and the test output".
3. The coder's answer ends with the branch its work was committed to (`sub-agent/<name>`). Do not fix the coder's work yourself.
4. Delegate to local-reviewer with one self-contained prompt: the criteria, the branch name, and "find the default branch with git_status, read the branch with the git_diff tool using range = <default>..<branch>, check every criterion has a test, run the tests, do not edit any file; first line of your answer is APPROVE, REQUEST_CHANGES with a numbered list, or BLOCKED: <reason>".
5. On REQUEST_CHANGES: delegate to local-coder again with the original criteria, the branch to continue from, and the reviewer's list; then review again. At most twice.
6. On APPROVE: merge the branch with the git_merge tool (no_ff = true). Then delegate once more to local-reviewer: "run the verification command on the current tree and report its result lines; first line PASS or FAIL".
7. Report: verdict, merge commit or why nothing merged, the criteria with their evidence.

Every prompt to a worker is self-contained: the worker sees none of this conversation.
EOF
git add .chatty/teams && git commit -q -m "Add the bank-fix team"
```

Two rules in there carry most of the weight. *Every prompt to a worker is self-contained* — a worker starts with an empty conversation, so anything the leader does not put in the prompt does not exist. And *do not fix the coder's work yourself* — the leader could not anyway (no shell, no writes), but a model will try to reason its way around a missing tool unless told not to.

### 4. Run it again with your team

The task is shorter now that the team file carries the verification:

```bash
chatty-tui --team bank-fix --headless --auto-approve --ollama --model qwen3:14b \
  -m "Fix Account.withdraw in bank.py. Acceptance criteria: 1. withdraw raises ValueError when the amount is not positive. 2. withdraw raises ValueError when the amount exceeds the balance. 3. Otherwise withdraw subtracts the amount and returns the new balance. 4. tests/test_bank.py gains a test for each of criteria 1-3. 5. test_deposit keeps passing."
```

Same shape of run, but now the evidence block carries the verification result without the task mentioning the command, and the `list_agents` card the leader reads says **Model: qwen2.5-coder:14b. Tool profile: coder.** for the coder.

### 5. Change the team and watch the difference

Now that a run is reproducible, experiment. Each is one edit to `team.json`:

- **Add a role.** A third worker `local-tester` with `"tools": "reviewer"` and a preamble that only writes and runs a negative probe, called by the playbook between coder and reviewer. Roles are what you make of the preamble; the tool profile just bounds them.
- **Starve the coder.** Set its `max_agent_turns` to `8` and watch it run out mid-task; the leader is told the delegation failed. What it does next is up to your playbook — add a line for it and see whether the leader follows.
- **Take the shell away from the reviewer.** `"disable_tools": ["shell"]` on top of `"tools": "reviewer"`: it can still read the diff but no longer run the tests — and Chatty skips the verification command for it, since a worker that could not run anything has no build to check.

Keep `SKILL.md` and `team.json` in the repository. The team is part of how the project gets worked on, and a run with a different team file is a different experiment.

## Verify

- After step 2: a merge commit on `main`, four passing tests, `sub-agent/local-coder-0` present, and an `evidence` block ending in `exit code 0` in the coder's report.
- After step 4: the same, produced by `bank-fix`, with the coder running `qwen2.5-coder:14b`.
- `chatty-tui --team no-such-team …` fails immediately, naming the two directories it looked in and the presets it knows — your team is found because it sits in the first of those directories.

## Common issues

- **The leader edits files itself or "cannot find" the coder.** Its first turn did not follow the skill. Check `skill` in `team.json` matches the `SKILL.md` name and that the file is beside `team.json`; run without `--headless` to read what the leader saw.
- **The coder's evidence block is missing.** The coder committed nothing (it reported but did not write, or wrote in the wrong place). A worker that changes nothing gets no block, on purpose — there is nothing to merge. Tighten the coder's preamble: *work only in your own workspace*.
- **`git_merge` reports a conflict.** The leader stops and lists the files; it never resolves conflicts. Merge by hand, or reset and run again.
- **The reviewer approves a branch that does not do the job.** Make the reviewer reproduce something: the preset's reviewer is told to *revert one line of the fix and confirm the new test fails*. A reviewer that only reads will approve on the coder's word.
- **A worker stalls at the model server.** Two workers on one Ollama server queue rather than run at once — that is the endpoint budget protecting a local model from thrashing — so a run with a coder and a reviewer is sequential by design. Only the wall clock is affected.

## Next

- [Sub-agents › Teams](./sub-agents.md#teams) — where teams are searched for and how `--model` / `--tools` / `--preamble` override the leader.
- [Security & approvals](./security.md) — what `--auto-approve` lets a worker do and how to run a team attended instead.
- The built-in `coder-reviewer` playbook is a stricter version of yours — a coder that turns every criterion into a test first and reports a criterion-to-evidence table, a reviewer told to *verify, do not trust, verdict first* and to reproduce a negative probe, a bounded REQUEST_CHANGES loop, and a final verification pass on the merged tree. The developer page [Teams](../dev/architecture/a2a-and-wasm-modules.md#teams) shows where it lives in the source; borrow from it once your own team works.
