# Tutorial: your first named worker

**When to read this:** You have used the agent and want to see delegation for real — one named worker with its own model and a role, doing one job for a leader — before building a whole team. Twenty minutes, nothing to install beyond Chatty and Ollama.

## What you will end up with

A worker called `local-reviewer` that only reads and runs tests, a leader that finds it by name and hands it a review, and a transcript that shows the hand-off, the worker's report, and the leader relaying it. Everything runs on your machine.

## Prerequisites

- [Ollama](https://ollama.com) running locally with two models pulled: one for the leader and one for the worker. This page uses `qwen3:14b` for the leader and `qwen2.5-coder:14b` for the worker. They need to fit in memory together (about 20 GB of GPU or unified memory); on a smaller machine use one model for both and read the note at the end.
- The terminal app, `chatty-tui` ([install](./terminal.md#install)). Delegation works from the desktop app too, but the terminal shows every hand-off as it happens, which is what you want the first time.
- Linux or macOS. The local worker broker is not available on Windows yet.

## Steps

### 1. Make a small project to review

Any folder with some code will do. To follow along exactly, create this one — a bank account with a bug in `withdraw` and a test suite that does not catch it:

```bash
mkdir -p ~/bank/tests && cd ~/bank && git init -b main
cat > bank.py <<'EOF'
class Account:
    def __init__(self, balance=0):
        self.balance = balance

    def deposit(self, amount):
        self.balance += amount
        return self.balance

    def withdraw(self, amount):
        # bug: overdraft is allowed and negative amounts are accepted
        self.balance -= amount
        return self.balance
EOF
touch tests/__init__.py
cat > tests/test_bank.py <<'EOF'
import unittest
from bank import Account

class TestBank(unittest.TestCase):
    def test_deposit(self):
        a = Account(10)
        self.assertEqual(a.deposit(5), 15)
EOF
printf '__pycache__/\n' > .gitignore
git add -A && git commit -q -m "Bank account with a withdraw bug"
python3 -m unittest discover -s tests -t . && echo "one test, passes, bug not caught"
```

It has to be a git repository: a worker that edits files gets its own branch, and even a read-only worker is told which branch it is looking at.

### 2. Declare the worker

Named workers live in your module settings file, next to the rest of Chatty's settings ([where that is on your platform](./advanced.md#where-chatty-stores-data)). On Linux:

```bash
mkdir -p ~/.config/chatty
cat > ~/.config/chatty/module_settings.json <<'EOF'
{
  "virtual_agents": [
    {
      "name": "local-reviewer",
      "model": "qwen2.5-coder:14b",
      "tools": "reviewer",
      "preamble": "You are the reviewer. Read the code you are pointed at, run the tests, and report what you found as a numbered list; never edit files."
    }
  ]
}
EOF
```

If the file already exists, add the `virtual_agents` array to it rather than replacing it. There is no settings page for this yet.

Four fields, and each one matters:

- **`name`** is how the leader will address it. Keep the `local-` prefix by convention: the leader's tools list where each agent runs, and `local` means this machine.
- **`model`** is the worker's own model. It does not have to be the leader's.
- **`tools`** is the role. `reviewer` can read, search, look at diffs and run commands — enough to run a test suite — and cannot write, commit or delegate. The other two roles are `coder` and `coordinator`; [Sub-agents](./sub-agents.md#named-workers-and-roles) has the full table.
- **`preamble`** is the standing instruction. Its first sentence is what the leader sees on the worker's card, so lead with the role.

### 3. Run a leader that can delegate

```bash
cd ~/bank
chatty-tui --broker --ollama --model qwen3:14b --auto-approve
```

`--broker` is the flag that makes the roster reachable: it starts the local worker broker for this session and publishes every worker in your module settings. `--ollama` finds your local models without any other setup, `--model` picks the leader's, and `--auto-approve` lets the worker run its tools (the test suite) without a prompt you would not be able to answer on its behalf.

The welcome screen lists what is active — model, workspace, tool groups — and the status bar shows the branch, `main`.

### 4. Ask for the review

Type this as your first message:

```
Use list_agents to see who is on the team, then ask local-reviewer to review
bank.py and tests/test_bank.py and report any bugs and missing tests. Do not
read or fix the code yourself; relay the reviewer's findings.
```

Watch the transcript. First the leader looks at the roster — the folded tool line reads `list_agents`, and `/verbose` shows what it saw:

```json
{
  "agents": [
    {
      "name": "local-reviewer",
      "origin": "local",
      "kind": "worker",
      "description": "A chatty agent in its own process, with its own workspace. Delegate a self-contained task to it and it works autonomously and reports back. Model: qwen2.5-coder:14b. Tool profile: reviewer. Role: You are the reviewer.",
      "enabled": true
    }
  ],
  "total": 1
}
```

That card is all the leader knows about the worker — the model, the role, and the first sentence of your preamble — and it is enough to choose by. Then the hand-off:

```
⟳ invoke_agent
[agent: local-reviewer] [local] ⟳ running
```

While that line is spinning, a second `chatty-tui` process is alive on your machine with `--model qwen2.5-coder:14b --tools reviewer` and your preamble, working through the files and running the tests in its own workspace. Its progress streams into the leader's transcript as it goes. When it finishes, the line flips to `✓ completed`, its report becomes the tool result, and the leader writes its answer from that — which should name the two problems in `withdraw` (negative amounts, overdraft) and the tests that are missing for them.

### 5. Look at what the worker left behind

```bash
git status          # clean: main is untouched
git branch          # main, sub-agent/local-reviewer-0
ls .chatty/worktrees/   # local-reviewer-0
```

Every worker gets its own copy of the tree on its own branch, `sub-agent/<worker>-<n>` (the number counts that worker's delegations in the session), whether or not its role can write. The reviewer's copy is where it ran the tests. It committed nothing, so the reply carries no `evidence` block — that block only exists when there is something to merge. Give a `coder` role the same task and the reply ends with one: the branch, its commit count and a diff summary. That is what the next tutorial builds on.

Worktrees and branches are left for you to inspect and are never removed behind your back. When you are done with one: `git worktree remove .chatty/worktrees/local-reviewer-0 && git branch -D sub-agent/local-reviewer-0` (in that order — git will not delete a branch a worktree still has checked out).

## Verify

- `list_agents` showed `local-reviewer` with **Model: qwen2.5-coder:14b. Tool profile: reviewer.** on its card.
- The transcript shows `invoke_agent` → `[agent: local-reviewer] [local] ⟳ running` → `✓ completed`.
- The leader's answer reports the overdraft and negative-amount bugs and the missing tests; `git status` is clean and `git branch` shows `sub-agent/local-reviewer-0`.

## Common issues

- **`list_agents` shows nothing, or only `local-agent`.** The leader was started without `--broker`, or the settings file is not where Chatty reads it. `chatty-tui` prints the config directory in `--help` under *Prerequisites*; check the file is there and is valid JSON.
- **The worker fails with an Ollama error such as `llama runner process has terminated`.** The two models do not fit in memory together and Ollama fell back to CPU or ran out. Use one model for both (`"model"` on the worker set to the same tag as `--model`), or pick a smaller worker such as `qwen3:4b`.
- **The leader thinks for a very long time before delegating.** `qwen3` reasons before every answer. Either accept it, or use a model that does not (for example the coder model for the leader as well), or turn thinking off for that model in your desktop model settings by adding `"extra_params": {"think": "false"}` to its entry in `models.json`.
- **The worker asks a question.** It cannot see your conversation, so the leader answers from the task if it can and otherwise the question reaches you as a clarifying prompt that takes over the input row. Answer it and the worker continues.

## Next

- [Tutorial: a small agentic team](./tutorial-team.md) — a coder and a reviewer, a verification command, and a leader that merges only on APPROVE.
- [Sub-agents › Named workers and roles](./sub-agents.md#named-workers-and-roles) — every field and every role.
