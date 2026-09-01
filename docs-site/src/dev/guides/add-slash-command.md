---
audience: [contributor, agent]
source_files:
  - crates/chatty-gpui/src/chatty/views/chat_input/slash.rs
  - crates/chatty-gpui/src/chatty/controllers/app_controller/slash_commands.rs
  - crates/chatty-tui/src/engine/commands.rs
related:
  - ./dev/reference/slash-commands.md
  - ./dev/guides/add-tool.md
---

# Add a slash command

**When to read this:** Expose a new `/command` in the chat input. Desktop (GPUI) and
terminal (TUI) register commands in **different** files — there is no shared
command table.

Full lookup table: [slash commands reference](../reference/slash-commands.md).

## Two command shapes

| Shape | Example | GPUI wiring | TUI wiring |
|-------|---------|-------------|------------|
| Immediate (no args) | `/compact` | Picker `execute_immediately: true` → `ChatInputEvent::SlashCommandSelected` → `handle_slash_command` | `Command` variant, no `Option<String>` |
| Arg-based | `/agent <prompt>` | Picker inserts `/agent ` (does not execute) → user sends → `try_handle_arg_slash_command` | `Command::Agent(Option<String>)` |

Skills from `.claude/skills/` appear in both pickers with a skill badge. Do not
register a skill as a built-in command.

## GPUI (desktop)

1. Add a `SlashCommand` to `SLASH_COMMANDS` in
   `crates/chatty-gpui/src/chatty/views/chat_input/slash.rs`
   (`command`, `description`, `insert_text`, `execute_immediately`).
2. Immediate commands: add a match arm in
   `ChattyApp::handle_slash_command`
   (`crates/chatty-gpui/src/chatty/controllers/app_controller/slash_commands.rs`).
3. Arg-based commands: handle the prefix in
   `ChattyApp::try_handle_arg_slash_command` in the same file. Return `true` so
   the message is **not** forwarded to the LLM.
4. `ChattyApp` already routes `ChatInputEvent::SlashCommandSelected` and
   intercepts `Send` for arg-based commands — no extra subscribe.

## TUI (terminal)

1. Add a `Command` variant and a `parse_command` match arm in
   `crates/chatty-tui/src/engine/commands.rs`.
2. Handle the variant in the engine (same file / `ChatEngine`).
3. The TUI slash menu builds from the same `Command` parse list plus skills
   (`crates/chatty-tui/src/ui/input.rs`). If the new command should appear in
   the `/` picker, add it there too.
4. Add a parse test in `crates/chatty-tui/src/engine/helpers.rs`
   (`parses_new_slash_commands`).

TUI-only today: `/model`, `/tools`, `/modules`, `/update`, `/quit`, `/exit`.
GPUI has no `/quit` (window chrome handles that).

## Parity checklist

- [ ] Same command string and argument syntax in both UIs, or document the
      exception in [slash-commands.md](../reference/slash-commands.md)
      (`scripts/gen-docs-reference.sh`)
- [ ] Immediate vs arg-based behavior matches
- [ ] Unknown `/foo` is ignored (TUI) or logged (GPUI) — never sent as a user
      message if you claimed it as a command
- [ ] `make docs-gen` after changing the reference table
