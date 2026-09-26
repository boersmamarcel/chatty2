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
- `write`, `resize`, `kill`; dropping the handle kills the child's process
  group and joins the PTY thread.
- `with_term` reads the grid under its lock (for the renderer);
  `generation()` changes whenever content or size does.
- `snapshot(Region) -> TerminalText`: the screen, or the last N rows of
  scrollback, as plain text.

`PtyWrite` and `TextAreaSizeRequest` replies are written back to the PTY
inside the crate, so programs that query the terminal work headless.

## Tests

`cargo test -p chatty-terminal` spawns real shells under a PTY (Unix only).
The Windows ConPTY path is compiled by CI's Windows check but not run.
