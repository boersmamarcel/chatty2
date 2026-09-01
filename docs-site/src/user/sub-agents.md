# Sub-agents

Delegate a subtask to a headless `chatty-tui` process. The child runs in the
background and returns its result to the parent.

## Why use them

- **Parallelism** — independent subtasks run at the same time
- **Isolation** — separate context and tools; a bad child run does not pollute
  the parent transcript
- **Composition** — pipe one agent's output into the next
- **Specialization** — a focused prompt and toolset per child

`sub_agent` is a side-effecting tool: it follows the same approval mode as
shell and writes. See [Security](./security.md).

## From the desktop UI

Type `/agent <your prompt>` to launch a headless sub-agent inline.

## Via the `sub_agent` tool

The model can spawn children itself:

```
Task: "Refactor all modules and write tests for each"

→ sub_agent("Refactor authentication module and write tests")
→ sub_agent("Refactor billing module and write tests")
→ sub_agent("Refactor notifications module and write tests")
→ Parent merges results into a final summary
```

Each child is `chatty-tui --headless` with the same configured tools and
models. Children can spawn further children.

## From the terminal

```bash
# Single headless call
chatty-tui --headless -m "Summarize the changes in the last 5 commits"

# Pipe input
git diff HEAD~3 | chatty-tui --pipe

# Chain agents
chatty-tui --headless -m "List all TODO comments in src/" | chatty-tui --pipe
```

Install, modes, and keybindings: [Terminal interface](./terminal.md).

## Related

- [Agents](./agents.md)
- [Security](./security.md)
