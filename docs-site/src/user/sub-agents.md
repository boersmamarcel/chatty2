# Sub-agents

**When to read this:** Delegate parallel or isolated subtasks to headless
`chatty-tui` processes.

## Why sub-agents?

- **Parallelism** — independent subtasks run concurrently
- **Isolation** — each child has its own conversation context and tools; mistakes stay out of the parent transcript
- **Composition** — one agent's stdout can feed the next
- **Specialization** — focused prompt and toolset per child

## From the desktop UI

Type `/agent <your prompt>` to launch a headless sub-agent inline.

## Via the `sub_agent` tool

The parent model can spawn children programmatically:

```
Task: "Refactor all modules and write tests for each"

→ sub_agent("Refactor the authentication module and write tests")
→ sub_agent("Refactor the billing module and write tests")
→ sub_agent("Refactor the notifications module and write tests")
→ Parent merges results into a final summary
```

Each child is `chatty-tui --headless` with the same configured tools and
models. Children can spawn further children. `sub_agent` is an approval-gated
tool; see [Security & sandboxing](./security.md).

## From the terminal

```bash
# Single headless call
chatty-tui --headless -m "Summarize the changes in the last 5 commits"

# Pipe input
git diff HEAD~3 | chatty-tui --pipe

# Chain: first agent lists TODOs, second consumes that list
chatty-tui --headless -m "List all TODO comments in src/" | chatty-tui --pipe
```

Install, modes, and keybindings: [Terminal interface](./terminal.md).

## Related

- [Agents](./agents.md)
- [Agentic tools](./agentic-tools.md)
