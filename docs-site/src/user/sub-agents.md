# Sub-agents

**When to read this:** Delegate parallel or isolated subtasks to headless `chatty-tui` agents.

## Why sub-agents?

- **Parallelism** — independent subtasks run concurrently
- **Isolation** — separate context and tools; mistakes don't pollute the parent conversation
- **Composition** — chain agents where one output feeds the next
- **Specialization** — focused prompts and toolsets per sub-agent

## From the desktop UI

Type `/agent <your prompt>` to launch a headless sub-agent inline.

## Via the `sub_agent` tool

The LLM can spawn sub-agents programmatically:

```
Task: "Refactor all modules and write tests for each"

→ sub_agent("Refactor authentication module and write tests")
→ sub_agent("Refactor billing module and write tests")
→ sub_agent("Refactor notifications module and write tests")
→ Parent merges results into final summary
```

Each sub-agent is a full `chatty-tui --headless` process with the same configured tools and models. Sub-agents can spawn further sub-agents.

## From the terminal

```bash
# Single headless call
chatty-tui --headless -m "Summarize the changes in the last 5 commits"

# Pipe input
git diff HEAD~3 | chatty-tui --pipe

# Chain agents
chatty-tui --headless -m "List all TODO comments in src/" | chatty-tui --pipe
```

See [Terminal interface](./terminal.md) for install, modes, and keybindings.

## Related

- [Agents](./agents.md)
- [Security](./security.md) — approval flows for `sub_agent`
