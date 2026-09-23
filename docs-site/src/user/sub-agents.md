# Sub-agents

**When to read this:** You want the agent to split a job into parallel or isolated pieces, or you want to drive Chatty from scripts.

A sub-agent is a separate `chatty-tui` process the parent agent hands a task to, waits on, and reads the answer from. Each child has its own conversation, its own workspace copy, and the same configured models and tools. The parent asks for one through its `invoke_agent` tool, addressed to `local-agent`; the child reports its progress back over the local agent broker while it works. That broker hop was measured rather than assumed safe to add: paired against calling the child directly, its median added cost came out at 3 ms or less, well under the variance of a single model turn.

## Why bother?

- **Parallelism** — independent subtasks run at the same time.
- **Isolation** — a child's exploration and mistakes stay out of the parent transcript; only its final answer comes back.
- **Composition** — one agent's output can feed the next.
- **Focus** — each child gets one narrow prompt.

## Isolated file changes

When the workspace is a git repository, each spawned sub-agent works in its own `git worktree` on a new branch (`sub-agent/<name>`) instead of the shared workspace tree. That means two children editing the same file at the same time no longer silently overwrite each other — each keeps its own copy. The branch name is unique per repository: if `sub-agent/<name>` is already taken (for example, two sub-agent leaders on the same repo naming a worker the same thing), Chatty picks `sub-agent/<name>-2`, `-3`, and so on. When a child finishes, its changes are committed to its branch and the worktree is left on disk; the parent agent merges the branch to take the changes. The tool result includes a fenced `evidence` block naming the actual branch, its commit count, and a diff summary against the base branch — if the project declares a verification command, its exit code and last lines are in there too. A child that committed nothing gets no evidence block, since there's nothing to merge. If a worktree can't be created at all, the delegation fails with the reason rather than silently falling back to a shared tree.

> [!NOTE]
> If the workspace isn't a git repository, sub-agents fall back to sharing the parent's tree as before, so parallel children editing files can still collide.

## From the chat

Type `/agent <your prompt>` to launch a sub-agent inline and watch its progress in the transcript. `/agent <name> <prompt>` sends the prompt to a named remote agent you have installed as an [extension](./extensions.md) instead.

## Let the agent decide

With tools on ([Agents & tools](./agents-and-tools.md)) and the module runtime enabled ([Extensions](./extensions.md)), the parent can ask for children itself when a task splits cleanly:

```
Task: "Refactor all modules and write tests for each"

→ sub-agent: "Refactor the authentication module and write tests"
→ sub-agent: "Refactor the billing module and write tests"
→ sub-agent: "Refactor the notifications module and write tests"
→ Parent merges the results into a final summary
```

> [!NOTE]
> A child runs its own side-effect tools without prompting only when your approval mode is **Auto-approve All**. Under the other modes a child has no way to ask you, so keep its tasks to reading, searching and analysis. See [Security & sandboxing](./security.md).

> [!NOTE]
> Children of one model server queue rather than run all at once, so a local model is not thrashed by a wide fan-out. The limit is `default_endpoint_budget` in your module settings.

## From the terminal

```bash
# One headless call
chatty-tui --headless -m "Summarize the changes in the last 5 commits"

# Pipe input
git diff HEAD~3 | chatty-tui --pipe

# Chain: the first agent lists TODOs, the second works through the list
chatty-tui --headless -m "List all TODO comments in src/" | chatty-tui --pipe
```

`--auto-approve` skips approval prompts for scripted runs; `--enable` / `--disable` pick tool groups per run. Install, modes and keys: [Terminal interface](./terminal.md).

Add `--broker` to let that terminal leader spawn sub-agents of its own — without it, only the desktop app can delegate to `local-agent`:

```bash
chatty-tui --headless --broker -m "Refactor the auth module and write tests"
```

`--broker` works with `--headless`, `--pipe`, and the interactive TUI, and is Unix only.

## Named workers and roles

Out of the box every sub-agent is the same worker, `local-agent`: your default model with the parent's tools. You can instead declare **named workers**, each with its own model, a role that limits what it may do, and standing instructions. The parent sees each one as a card — its name, model, role and the first sentence of its instructions — and picks by reading, the same way it would choose a colleague. Nothing else about the parent changes: the prompt and the tool stay identical whether the roster is empty or five deep.

Declare them in `module_settings.json` next to your other settings ([where that is](./advanced.md)), under `virtual_agents`. There is no settings page for this yet, so edit the file and restart. This roster is used by the desktop app and by a terminal leader started with `--broker`:

```json
{
  "enabled": true,
  "virtual_agents": [
    {
      "name": "local-coder",
      "model": "qwen3:14b",
      "tools": "coder",
      "preamble": "You are the coder. Make the smallest change that makes the task's check pass, run it, and report the files you touched.",
      "max_agent_turns": 30
    },
    {
      "name": "local-reviewer",
      "model": "gemma3:27b",
      "tools": "reviewer",
      "preamble": "You are the reviewer. Read the diff, run the tests, and report what you found; never edit the tree."
    }
  ],
  "team": { "verification": "python3 -m pytest -q" }
}
```

| Field | What it does |
|-------|--------------|
| `name` | What the parent addresses; also the branch name, `sub-agent/<name>`. |
| `model` | This worker's model, matched the way `chatty-tui --model` matches (id, name, or part of the id). Leave it out to use the default model. A worker on a different model server is queued on that server's budget, not the parent's. |
| `tools` | A **role**: `coordinator`, `coder` or `reviewer`. A role is the worker's whole tool set; anything not in it — including every MCP tool — is gone, so a small model isn't handed fifty tool schemas before it can read a file. |
| `preamble` | Standing instructions, added to the worker's system prompt. Its first sentence is what the parent reads on the card, so lead with the role. |
| `max_agent_turns` | How many tool rounds this worker may take before it has to answer; the default of 10 is too few for a multi-step coding task. This is the worker's own budget — the parent's is **Max Agent Turns** under **Settings → Code Execution**. |
| `disable_tools` | The older, coarser switch: tool groups to remove (`shell`, `fs-write`, `git`, …). Ignored when `tools` is set. |

The three roles:

| Role | Can | Cannot |
|------|-----|--------|
| **`coordinator`** | Read files, search, read git history and diffs, delegate to other workers, merge a worker's branch, ask you a question. | Edit, run commands, commit. It hands work out; it never does it. |
| **`coder`** | Everything a coordinator can read, plus write files, run commands, commit on its own branch, run code, query data, remember and save skills. | Delegate further: a coder does not fan out. |
| **`reviewer`** | Read and search, read diffs (including another worker's branch), run commands so it can run the tests, query data. | Write, commit, or delegate. It reports; it never fixes. |

A role only ever removes tools: it cannot turn on a tool group you switched off under **Settings → Code Execution** (or with `--disable`).

`team.verification` is one command for the whole roster. When a worker finishes, Chatty commits its branch and then runs that command *itself* in the worker's tree — not through the worker — and puts the exit code and last lines into the `evidence` block the parent reads, next to the branch name, commit count and diff summary. That's the parent's proof that the coder's "tests pass" is true. It is skipped for a worker whose role has no shell, since that worker could not have built anything for it to check.

Every worker still needs a way to run its side-effect tools without asking you, so under any approval mode other than **Auto-approve All** keep the roster to reading roles or run your leader with `--auto-approve` from the terminal ([Security & approvals](./security.md)). The full field reference, including how a worker is metered and started, is on the developer page: [Named virtual agents](../dev/architecture/a2a-and-wasm-modules.md#local-agent--a-chatty-agent-in-its-own-process).

## Teams

`--team <id>` packages a roster like the one above with a leader and a verification command into one directory, so a run is reproducible and the leader has a role too: a named leader plus co-workers, each with its own model, role and standing instructions, defined once in `teams/<id>/team.json`. It implies `--broker`, and for that run the team's `agents` replace whatever `virtual_agents` your module settings declare.

One team ships built in, `coder-reviewer` — a leader that only delegates, a coder, and a reviewer who checks the diff against the default branch before the leader merges it:

```bash
chatty-tui --team coder-reviewer --headless --ollama --model qwen3:14b \
  -m "Fix the overdraft bug in src/account.py; the acceptance criterion is that tests/test_account.py passes."
```

`--model` (and `--tools` / `--preamble`) override the team's own leader settings when given. Bring your own team by adding `<workspace>/.chatty/teams/<id>/team.json`, which overrides both the built-in preset and any team of the same id under your data directory. File format and search order: [Teams](../dev/architecture/a2a-and-wasm-modules.md#teams).

## Next

- [Tutorial: your first named worker](./tutorial-named-worker.md) — twenty minutes, one reviewer, real hand-off
- [Tutorial: a small agentic team](./tutorial-team.md) — a team directory of your own, from the built-in preset to your own playbook
- [Terminal interface](./terminal.md)
- [Agents & tools](./agents-and-tools.md)
