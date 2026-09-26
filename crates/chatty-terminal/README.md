# chatty-terminal

The model half of Chatty's embedded terminal: a PTY running a shell and an
[`alacritty_terminal`](https://crates.io/crates/alacritty_terminal) `Term`
fed from it. No UI toolkit and no async runtime, so it runs headless (the
agent shell, Harbor runs) as well as under a view.

## Public surface

- `TerminalHandle::spawn(TerminalConfig)` starts the shell (`$SHELL` login +
  interactive on Unix, `pwsh`/`powershell` over ConPTY on Windows) and
  returns the handle plus the event channel. Drop the receiver to run with
  no subscriber.
- `spawn_with_tap(config, tap)` adds a byte-stream tap: a callback that
  sees every byte read from the PTY before the parser does.
- `TerminalConfig::scrollback` sets the lines of history kept (default
  10,000).
- `MarkScanner` (fed from the tap): OSC 133 `A`/`B`/`C`/`D` marks, chatty's
  private OSC 6973, and the `CleanText` between them (no escapes, `\r\n` as
  `\n`, redrawn lines as they ended, no wrapping at the terminal width).
- `commands() -> Vec<CommandRecord>`: the last 200 commands run at a shell
  with OSC 133 integration (text after line editing, output line range,
  exit code, start/finish time, `truncated` once the output left the
  scrollback), and `snapshot(Region::LastCommand)`: the last command's
  output (end kept, capped at 16 KiB) with `last_command` saying what ran
  and how it exited, or that integration is not active. Line numbers come
  from a shadow `Term` in the byte tap, stopped at the exact byte of each
  mark (see `src/commands.rs`).
- Shell integration (`TerminalConfig::shell_integration`, default on):
  bash and zsh started with default arguments load chatty's OSC 133
  snippets (`--rcfile` / a `ZDOTDIR` wrapper, `src/integration.rs`) after
  the user's own startup files, which are never edited. fish and PowerShell
  are unsupported and run without integration. With injection, bash is
  not a login shell to itself (`shopt login_shell` is off; our rcfile
  sources the login startup files instead), so a `.bash_profile` that
  branches on `login_shell` sees "not login".
- `write`, `resize`, `kill`; dropping the handle kills the child's process
  group and joins the PTY thread.
- `has_foreground_job()`: whether a program other than the shell holds the
  terminal's foreground (the PTY's foreground process group on Unix;
  `None` on Windows, where it cannot be told).
- `foreground_process_name()` and `current_dir()`: the foreground
  program's name and the shell's working directory, for tab titles (Linux,
  via `/proc`; `None` elsewhere).
- `with_term` reads the grid under its lock (for the renderer);
  `generation()` changes whenever content or size does.
- `snapshot(Region) -> TerminalText`: the screen, or the last N rows of
  scrollback, as plain text.

`PtyWrite` and `TextAreaSizeRequest` replies are written back to the PTY
inside the crate, so programs that query the terminal work headless.

## Tests

`cargo test -p chatty-terminal` spawns real shells under a PTY (Unix only).
The zsh test runs when `zsh` is on `PATH` (or `CHATTY_TEST_ZSH` names one)
and is skipped otherwise.
The Windows ConPTY path compiles with
`cargo check --target x86_64-pc-windows-msvc -p chatty-terminal` (verified
locally). CI's Windows check covers it once chatty-gpui or chatty-tui depend
on this crate; the Windows runtime is untested.
