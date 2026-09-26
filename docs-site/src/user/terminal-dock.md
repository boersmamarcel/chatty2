# Terminal dock

**When to read this:** You use the desktop app and want a shell next to the conversation — to run a build, look at `git status` or try what the agent just wrote — without leaving Chatty.

The desktop app has a terminal panel under the chat, placed and behaving like the terminal panel in VS Code or Zed. (For the terminal *app*, `chatty-tui`, see [Terminal interface](./terminal.md).)

## Open and close it

| Keys | What it does |
|---|---|
| **Ctrl+J** (Linux, Windows) / **Cmd+J** (macOS) | Open the dock and put the keyboard in its terminal; press again to close it and return to the message box |
| **Ctrl+`** | Same as Ctrl/Cmd+J |
| **Ctrl+Shift+`** | Open a new terminal tab |

The dock sits under the message box and spans the chat column only: the sidebar and the artifact panel keep their place beside it. The message box stays above the dock and keeps working while it is open.

- **Resize:** drag the dock's top edge. The height is remembered.
- **Maximise:** the square button on the right of the tab strip grows the dock to fill the chat column; the transcript is hidden while it is maximised and the message box stays. Click it again to restore.
- **Hide:** the chevron on the far right closes the dock, like Ctrl/Cmd+J.

Closing the dock doesn't stop anything: its terminals keep running and are there again when you reopen it.

## Tabs

Each terminal is a tab. **+** opens another, **×** closes one, and dragging a tab sideways changes the order. A tab is named after the program running in it and the folder it is in, for example `bash — chatty2` at the prompt or `cargo — chatty2` during a build; on macOS and Windows it always shows the shell and the folder the terminal started in. Hover a tab for the full title the program set, such as the shell's `user@host: path`.

When the shell exits (you typed `exit`, or it crashed), the tab stays, its name in italics followed by the exit status, for example *bash — chatty2* `exited 0`. One click on **×** closes it.

Closing a tab while a program is running in it, such as `vim` or a build, asks for confirmation first; closing a tab that is only showing its prompt doesn't. On Windows Chatty can't tell the two apart, so closing a running terminal always asks. Closing the last tab closes the dock.

## Where a terminal starts

A new terminal starts in the active conversation's working directory, the same folder the file explorer shows for it (set with the folder icon in the message box or `/cd`). Without one, it starts in the workspace directory from **Settings → Code Execution**, and without that, in your home folder.

Terminals belong to the window, not to a conversation. Switching to another conversation leaves them running and visible; only new terminals pick up the new conversation's folder.

Quitting Chatty ends every shell it started. Terminals are not restored the next time Chatty starts.

## Keyboard focus

Click in a terminal to type into it. Escape is passed to the program in the terminal (vim needs it) and does not move you out. To go back to the conversation, click the message box, or press Ctrl/Cmd+J to close the dock.

A few key combinations always go to Chatty, even while a terminal has the keyboard, so the dock can always be closed:

- Ctrl+J / Cmd+J
- Ctrl+`
- Ctrl+Shift+`

In a shell, Ctrl+J only types a newline, which Enter already does.

## Settings

**Settings → Terminal:**

| Setting | Default | Applies |
|---|---|---|
| Font Family | the theme's code font | at once |
| Font Size | the theme's code font size | at once |
| Shell | your login shell (`$SHELL` on macOS and Linux, PowerShell on Windows) | to new terminals |
| Scrollback Lines | 10,000 | to new terminals |
| Dock Height | 300 pixels | when the dock is next drawn |

Dragging the dock's top edge updates **Dock Height** too.
