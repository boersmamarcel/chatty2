# Sub-agents

**When to read this:** You want the agent to split a job into parallel or isolated pieces, or you want to drive Chatty from scripts.

A sub-agent is a headless `chatty-tui` process the parent agent launches with a task, waits on, and reads the answer from. Each child has its own conversation, the same configured models and tools, and can launch children of its own.

## Why bother?

- **Parallelism** — independent subtasks run at the same time.
- **Isolation** — a child's exploration and mistakes stay out of the parent transcript; only its final answer comes back.
- **Composition** — one agent's output can feed the next.
- **Focus** — each child gets one narrow prompt.

## From the chat

Type `/agent <your prompt>` to launch a sub-agent inline and watch its progress in the transcript. `/agent <name> <prompt>` sends the prompt to a named remote agent you have installed as an [extension](./extensions.md) instead.

## Let the agent decide

With tools on ([Agents & tools](./agents-and-tools.md)), the parent can spawn children itself when a task splits cleanly:

```
Task: "Refactor all modules and write tests for each"

→ sub-agent: "Refactor the authentication module and write tests"
→ sub-agent: "Refactor the billing module and write tests"
→ sub-agent: "Refactor the notifications module and write tests"
→ Parent merges the results into a final summary
```

> [!NOTE]
> A child runs its own side-effect tools without prompting only when your approval mode is **Auto-approve All**. Under the other modes a headless child has no way to ask you, so keep its tasks to reading, searching and analysis. See [Security & sandboxing](./security.md).

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

## Next

- [Terminal interface](./terminal.md)
- [Agents & tools](./agents-and-tools.md)
