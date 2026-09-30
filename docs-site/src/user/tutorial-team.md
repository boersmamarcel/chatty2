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

### 1. Write the team

A team is three things: the agents (each an agent spec), a `team.json` naming the leader and the roster, and the playbook. Write the three agents as specs, and a team directory naming them, in the project:

```bash
mkdir -p .chatty/agents .chatty/teams/bank-fix
cat > .chatty/agents/bank-lead.toml <<'EOF'
[agent]
name = "bank-lead"
preamble = "You lead a two-worker team and edit nothing yourself. Restate the task as acceptance criteria, delegate the change to local-coder, have local-reviewer check the coder's branch, and merge only on APPROVE."

[tools]
profile = "coordinator"
EOF
cat > .chatty/agents/local-coder.toml <<'EOF'
[agent]
name = "local-coder"
model = "qwen2.5-coder:14b"
preamble = "You are the coder. Work only in your own workspace, write a test for each acceptance criterion before making it pass, run the tests, do not commit, and report the files you changed with the test output."

[tools]
profile = "coder"

[budget]
max_agent_turns = 30
EOF
cat > .chatty/agents/local-reviewer.toml <<'EOF'
[agent]
name = "local-reviewer"
model = "qwen2.5-coder:14b"
preamble = "You are the reviewer. Read the branch's diff yourself, run the tests yourself, never edit the tree. The first line of your answer is APPROVE, REQUEST_CHANGES or BLOCKED."

[tools]
profile = "reviewer"
EOF
cat > .chatty/teams/bank-fix/team.json <<'EOF'
{
  "leader": "bank-lead",
  "agents": ["local-coder", "local-reviewer"],
  "verification": "python3 -m unittest discover -s tests -t . -v",
  "skill": "bank-fix",
  "max_agent_turns": 50
}
EOF
```

What each field does, and why:

| Field | Here | Why |
|-------|------|-----|
| `agent.model` in the worker specs | `qwen2.5-coder:14b` on both workers | A coding model for the code; the leader keeps `qwen3:14b` for planning. Each worker is metered on its own model's server, so a worker on another machine would not queue behind the leader. |
| `budget.max_agent_turns` in `local-coder.toml` | `30` | A cap on the coder's tool rounds. Without one a worker has no turn cap and a 30-minute time budget (`--max-duration`). This is the coder's own budget. |
| `verification` | in the file | So the task no longer has to say it. Chatty runs this in each worker's tree at the end and puts the result in the evidence block. |
| `max_agent_turns` (top level) | `50` | The **leader's** budget for the run. A delegating leader burns a turn per hand-off and per tool call. Without it the leader has no turn cap and a 30-minute time budget. |
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
git add .chatty/agents .chatty/teams && git commit -q -m "Add the bank-fix team"
```

Two rules in there carry most of the weight. *Every prompt to a worker is self-contained* — a worker starts with an empty conversation, so anything the leader does not put in the prompt does not exist. And *do not fix the coder's work yourself* — the leader could not anyway (no shell, no writes), but a model will try to reason its way around a missing tool unless told not to.

### 2. Run it

Give the leader the task. The team file carries the verification command, so the task does not need to:

```bash
chatty-tui --team bank-fix --headless --auto-approve --ollama --model qwen3:14b \
  -m "Fix Account.withdraw in bank.py. Acceptance criteria: 1. withdraw raises ValueError when the amount is not positive. 2. withdraw raises ValueError when the amount exceeds the balance. 3. Otherwise withdraw subtracts the amount and returns the new balance. 4. tests/test_bank.py gains a test for each of criteria 1-3. 5. test_deposit keeps passing."
```

`--team` implies `--broker`. The `list_agents` card the leader reads says **Model: qwen2.5-coder:14b. Tool profile: coder.** for the coder. The task states the acceptance criteria explicitly because that is what the playbook expects: vague tasks make vague teams.

`--headless` prints only the leader's final answer on stdout; the hand-offs go to stderr as they happen. To watch both, drop `--headless` and `-m` and type the task into the interactive app instead.

### 3. Read the run

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

### 4. Change the team and watch the difference

Now that a run is reproducible, experiment. Each is one edit to a spec or `team.json`:

- **Add a role.** A third worker, `.chatty/agents/local-tester.toml` with `profile = "reviewer"`, listed in `team.json`'s `agents`, and a preamble that only writes and runs a negative probe, called by the playbook between coder and reviewer. Roles are what you make of the preamble; the tool profile just bounds them.
- **Starve the coder.** Set its spec's `budget.max_agent_turns` to `8` and watch it run out mid-task; the leader is told the delegation failed. What it does next is up to your playbook — add a line for it and see whether the leader follows.
- **Take the shell away from the reviewer.** `disable = ["shell"]` under its spec's `[tools]`, on top of `profile = "reviewer"`: it can still read the diff but no longer run the tests — and Chatty skips the verification command for it, since a worker that could not run anything has no build to check.

Keep the specs, `SKILL.md` and `team.json` in the repository. The team is part of how the project gets worked on, and a run with a different team file is a different experiment.

## Verify

- After step 2: a merge commit on `main`, four passing tests, `sub-agent/local-coder-0` present, and an `evidence` block ending in `exit code 0` in the coder's report, with the coder running `qwen2.5-coder:14b`.
- `chatty-tui --team no-such-team …` fails immediately, naming the two directories it looked in and the presets it knows. Your team is found because it sits in the first of those directories.

## Common issues

- **The leader edits files itself or "cannot find" the coder.** Its first turn did not follow the skill. Check `skill` in `team.json` matches the `SKILL.md` name and that the file is beside `team.json`; run without `--headless` to read what the leader saw.
- **The coder's evidence block is missing.** The coder committed nothing (it reported but did not write, or wrote in the wrong place). A worker that changes nothing gets no block, on purpose — there is nothing to merge. Tighten the coder's preamble: *work only in your own workspace*.
- **`git_merge` reports a conflict.** The leader stops and lists the files; it never resolves conflicts. Merge by hand, or reset and run again.
- **The reviewer approves a branch that does not do the job.** Make the reviewer reproduce something: tell it to *revert one line of the fix and confirm the new test fails*. A reviewer that only reads will approve on the coder's word.
- **A worker stalls at the model server.** Two workers on one Ollama server queue rather than run at once — that is the endpoint budget protecting a local model from thrashing — so a run with a coder and a reviewer is sequential by design. Only the wall clock is affected.

## A built-in coding team: `fix-and-verify`

> Experimental, like every team that ships built in.

If you want the result of this tutorial without writing a team, `fix-and-verify` ships with Chatty. Its lead restates the task as acceptance criteria, `fix-coder` fixes the bug in its own worktree, and Chatty runs the project's test command in that worktree. `code-reviewer` has no shell. It reads the real diff and the test output Chatty wrote, and never takes the coder's word that the tests pass. The lead merges only on `APPROVE` with exit code 0, after at most one fix round. Try it on the sample project, a small invoice module with one failing test:

```bash
mkdir -p invoice/tests && cd invoice
curl -LO https://boersmamarcel.github.io/chatty2/assets/samples/invoice/invoice.py
curl -Lo tests/test_invoice.py https://boersmamarcel.github.io/chatty2/assets/samples/invoice/tests/test_invoice.py
touch tests/__init__.py && printf '__pycache__/\n' > .gitignore
git init -q -b main && git add -A && git commit -qm "Invoice module with a failing test"
chatty-tui --team fix-and-verify --headless --auto-approve --model <model> \
  -m "The project's tests fail. Find the bug and fix it without changing the tests."
```

The team's `verification` is this sample's command, `python3 -m unittest discover -s tests -t . -v`. For your own project, put a `.chatty/teams/fix-and-verify/team.json` with the same `leader` and `agents` and your own test command. It shadows the built-in team, and the specs stay the built-in ones.

On the desktop, `/agent fix-lead <task>` runs the same team, and the swarm tree shows the lead, the coder's run and the review as they happen. `fix-lead` is this team's leader, so the coder's evidence block still carries the team's own test command, `python3 -m unittest discover -s tests -t . -v` (or your own, from a `.chatty/teams/fix-and-verify/team.json` in the workspace) — exactly as `--team fix-and-verify` runs it. On the desktop the lead is a worker too, so it merges into its own branch, `sub-agent/fix-lead-0`, and that branch is what you merge.

![The fix-and-verify team in the swarm tree: the lead, fix-coder and code-reviewer](../assets/screenshots/fix-and-verify-tree.png)

## A built-in architecture-review team: `architecture-review`

> Experimental, like every team that ships built in. Nothing has measured yet whether it writes a better document than one agent does.

`architecture-review` writes one architecture document from the code in your workspace, either an ADR (one decision between alternatives, in `docs/adr/ADR-NNNN-slug.md`) or a design doc (how a component works, in `docs/design/<component>.md`). The lead picks which from your request. `arch-proposer` owns the document: it reads the code, writes the document and answers the reviews. Then three reviewers read it cold: `arch-maint-reviewer` for maintainability, `arch-sec-reviewer` for security, and `arch-devils-advocate`, who argues for the best alternative the document turned down. Every round starts fresh reviewers with new personas, and they see neither the earlier rounds nor each other. Every finding is `must-fix`, `should-fix` or `nit`. The proposer checks each finding against the code, then fixes it or rejects it with evidence. A question only you can decide goes to you: the lead asks it during the run.

Rounds repeat until one finds no `must-fix` at all, not counting any the proposer rejected with evidence. The run stops early, and says **not converged**, when two rounds in a row do not lower the number of must-fix findings, when a fixed problem comes back, or after 10 rounds. A last wording pass is checked by `arch-verifier`, which makes sure the pass changed no substance. An ADR keeps `status: proposed`, because the decision is yours. Beside the document, `<document>.review.md` lists the must-fix count of each round, whether the review converged, your decisions, and the questions still open for you.

Run it from the root of a git repository, with a clean tree:

```bash
chatty-tui --team architecture-review --auto-approve \
  -m "Write an ADR for adding a per-agent private-network flag to A2A remote agents."
```

Leave `--headless` off so the lead can ask you its questions. In a headless run with `--disable ask_user`, the questions go to the review file.

**Models, and what they cost.** This is the one built-in team that names its models. The lead, the proposer and the three reviewers run `anthropic/claude-opus-5`, and the verifier runs the cheaper `anthropic/claude-sonnet-5`, both through OpenRouter. A run costs hosted-model tokens for all six agents, over as many as 10 rounds. Without an OpenRouter key the team does not start: the error lists each agent with the model it names, and the two ways to run it on a model you have. They follow.

**One model for the whole team.** `--model` runs every agent of a `--team` run on that model, including agents whose spec names a model of its own:

```bash
chatty-tui --team architecture-review --model <model> --auto-approve \
  -m "Write an ADR for adding a per-agent private-network flag to A2A remote agents."
```

### Run it on Azure OpenAI

1. **Configure the provider.** In **Settings → Providers → Azure OpenAI**, enter your resource's endpoint URL and either an API key or **Use Entra ID instead of a key**. With Entra ID, Chatty signs in the way the Azure SDK does: a service principal from `AZURE_CLIENT_ID`, `AZURE_TENANT_ID` and `AZURE_CLIENT_SECRET`; workload identity from `AZURE_FEDERATED_TOKEN_FILE`; otherwise your `az login` (or `azd auth login`) session. See [Providers & models](./providers-and-models.md#azure-openai).
2. **Add a model per deployment.** Its identifier is the deployment name. A GPT-5-class reasoning deployment rejects a temperature, so turn its **Temperature** capability off: set `"supports_temperature": false` on that model in `models.json` (the desktop has no switch for it yet, and keeps the value when you edit the model).
3. **Run the whole team on that deployment:**

   ```bash
   chatty-tui --team architecture-review --model <deployment> --auto-approve \
     -m "Write an ADR for adding a per-agent private-network flag to A2A remote agents."
   ```

   `<deployment>` is the model's id, its name, or part of the deployment name, as for any `--model`.

**A different model per agent.** To keep a strong model for most agents and a cheaper one for the verifier, say, *shadow* the agents whose model you change: put a spec with the same name in `<workspace>/.chatty/agents/`, which replaces the built-in one for that workspace. Copy the agent's spec from the Chatty source (`crates/chatty-core/agents/arch-*.toml`), then change its `model` line to one of your models, or delete it so the agent runs your default model. The rest of the spec stays as it was:

```toml
[agent]
name = "arch-verifier"
model = "my-cheaper-deployment"   # or leave the line out to use your default model
# ... the rest of the built-in spec, unchanged
```

Don't combine this with `--model`, which replaces every agent's model for the run, shadowed ones included. Shadowing all six agents with a local model makes the team cheap to try, but the reviews are weaker.

## Next

- [Sub-agents › Teams](./sub-agents.md#teams) — where teams are searched for and how `--model` / `--tools` / `--preamble` override the leader.
- [Security & approvals](./security.md) — what `--auto-approve` lets a worker do and how to run a team attended instead.
- [Tutorial: from one agent to a team](./tutorial-swarm.md): the desktop walkthrough with the shipped `data-analysis` team, approvals, the per-model bill and mixed models.
