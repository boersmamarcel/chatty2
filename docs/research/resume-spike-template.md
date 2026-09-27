# Resume spike: the frozen prompts

**When to read this:** You are running or analysing the resume spike (RC-1,
AGE-650; RC-2, AGE-651) and need to know exactly what each arm's follow-up
worker was told.

This file is frozen. It was committed before any real run, and the test
`rebrief_template_is_frozen` (`crates/chatty-tui/tests/resume_spike.rs`) pins
its SHA-256, so any edit fails CI on purpose. The harness
(`scripts/resume-spike/spike.py`) reads both prompts from the two `text`
blocks below, so what is pinned here is what is sent. Changing a prompt means
a new template, a new hash, and results that do not pool with earlier ones.

Spec: vault `dev/projects/fabric-resumable-conversations.md` §3.1.

## Arm R, re-brief

A fresh worker, in a fresh worktree checked out at the first task's result,
gets this as its only message. It has no conversation behind it.

```text
You are picking up work another worker started in this repository. Its task, what it reported, and the commits it left are below. The repository in your workspace already holds that work.

## The original task

{task}

## What the first worker reported

{summary}

## Commits from that work

{commits}

## Your task now

{follow_up}
```

## Arm C, cold resume

The first worker's own conversation, restored (`chatty-tui --headless
--restore`), in the first worker's own worktree, gets this as its next
message.

```text
Since your last task ended, these commits landed on your branch:

{commits_since}

Your next task:

{follow_up}
```

## How the slots are filled

| Slot | Filled with |
| -- | -- |
| `{task}` | The task's `task` text, verbatim: the same message the first worker got. |
| `{summary}` | The leader's summary of the first result. In the spike this is the first worker's final answer verbatim (what a leader receives from `invoke_agent`), cut to its last 4000 characters with a `[…]` marker when longer. No extra model call writes it. That gives arm R everything the worker said at no cost, which favours arm R and so makes the comparison conservative for arm C. |
| `{commits}` | `git log --oneline <base>..<first result>`, oldest first: the first worker's own commits, if any, and the harness's commit of what it left uncommitted. `(none)` when empty. |
| `{commits_since}` | `git log --oneline <end of the first run>..<first result>`, oldest first: what landed after the conversation ended, which in the spike is only the harness's commit of the worker's uncommitted changes. `(none)` when empty. |
| `{follow_up}` | The task's `follow_up` text, verbatim: a review finding to fix, or a requirement change. |

A slot is replaced by plain substitution of the literal `{name}` text; nothing
else in a prompt is interpreted.
