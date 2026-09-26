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

## The Agent tab

The first tab, with the robot icon, is always **Agent**: the shell the agent runs its commands in for the active conversation. It shows every command the agent ran with its full output, not just the part a tool row has room for, and you can type in it too: it is one shell, shared by you and the agent.

- **Switching conversations** switches the Agent tab to that conversation's shell. Your own tabs stay.
- **Before the agent used its shell** the tab says so and offers **Start shell**, so you can prepare the environment (activate a virtualenv, `cd`, export a variable) before you ask. It starts the same shell the agent then uses. With code execution off in **Settings → Code Execution** the agent has no shell and the tab says that instead.
- **Its ×** hides the dock; the Agent tab can't be closed, and the shell keeps running.
- **Which lines were the agent's:** a small blue bar in the left margin marks each command the agent ran. Commands you typed have none.
- **Status:** next to its name the tab says what the shell is doing: *idle*, *agent running `cargo test`*, *you typing*, or *you running …*.
- **Sandbox:** when the agent's shell runs in the sandbox, the tab says *sandboxed* (or *sandboxed · no network* with network isolation on). What you type there runs in the same sandbox.

**Sharing the keyboard.** You can type at any time. While the agent's command runs, what you type goes to that program: answer its prompt, or press Ctrl+C to stop it (the agent then gets the interrupted output and exit code 130). While you have a half-typed line at the prompt, the agent's next command waits for you: the tab turns amber and says **Agent waiting for you** with the command it wants to run. Press Enter or clear the line (Ctrl+U) and it goes ahead. If you don't within two seconds, the agent is told the terminal is busy and can try again.

**Show in terminal.** Each shell command row in the conversation has a terminal button: it opens the dock on the Agent tab, scrolled to that command. If the same command ran more than once, it shows the latest run; a command that has scrolled out of the scrollback, or ran in a shell that has since restarted, can't be shown.

The agent can read its own tab with `terminal_read` without you sharing it, so it can see what you ran there ("what did I just run?"). A hidden-input prompt, such as a password prompt, is never read.

## Where a terminal starts

A new terminal starts in the active conversation's working directory, the same folder the file explorer shows for it (set with the folder icon in the message box or `/cd`). Without one, it starts in the workspace directory from **Settings → Code Execution**, and without that, in your home folder.

Your terminals belong to the window, not to a conversation. Switching to another conversation leaves them running and visible; only new terminals pick up the new conversation's folder. The Agent tab is the exception: it always shows the active conversation's shell.

Quitting Chatty ends every shell it started. Terminals are not restored the next time Chatty starts.

## Keyboard focus

Click in a terminal to type into it. Escape is passed to the program in the terminal (vim needs it) and does not move you out. To go back to the conversation, click the message box, or press Ctrl/Cmd+J to close the dock.

A few key combinations always go to Chatty, even while a terminal has the keyboard, so the dock can always be closed:

- Ctrl+J / Cmd+J
- Ctrl+`
- Ctrl+Shift+`

In a shell, Ctrl+J only types a newline, which Enter already does.

## Sharing a terminal with the agent

Your terminals are yours: the agent can't see what is in them until you share one. Each tab has an eye icon; crossed out, the tab is not shared.

Click the eye on a tab that is not shared and choose how much the agent gets:

- **Read only:** the agent can read the screen and the scrollback, for example to answer *Why did that fail?* after a failing build.
- **Read + run:** the agent can also run commands in this terminal, each one only after you approve it.

Tick **Remember this, don't ask again** to skip the question from then on: the eye shares every tab at that level with one click. **Settings → Terminal → When sharing a terminal** changes or undoes that choice (**Ask** brings the question back).

Click the eye on a shared tab to stop sharing it at once.

A tab you haven't shared is still listed to the agent (its name and folder), so it can tell you it isn't shared and ask you to share it, but its contents are never read. While a terminal waits at a plain text password prompt (`sudo`, `ssh`, `su`, `passwd`, a script's `read -s`, or `gpg` with `--pinentry-mode loopback`), the agent gets "terminal is at a hidden-input prompt" instead of the screen, even from a shared tab. Full-screen passphrase boxes, such as the one `gpg` usually draws in the terminal (pinentry-curses), and pinentry windows outside the terminal are not caught: the passphrase itself is never on screen there, but the rest of the terminal can be read as at any other moment. Windows has no way to tell that a program is asking for a password, so on Windows don't leave a shared tab at a password prompt.

Every read shows up in the conversation as a tool row such as *Read terminal · bash — chatty2 · 42 lines*, naming the tab and how many lines the agent was given; the row's copy button gives the exact output. The tab it read lights up for a moment.

### Letting the agent run a command in your terminal

The agent normally runs commands in its own shell. When you want one to run in a tab of yours instead (your ssh session, your activated virtualenv, the dev server you are watching), share that tab as **Read + run** and ask for it, for example *Restart the dev server in my terminal*.

A tab shared this way is your own shell: it is not sandboxed and it holds your credentials. So every command the agent wants to run there first shows an approval card with the exact command, the tab's name and folder, and the line **Runs in your shell, not the sandbox.** Nothing is typed until you click **Approve**; **Deny** tells the agent no. This card always appears, one per command, whatever the approval mode under **Settings → Code Execution**: auto-approve never applies to your terminals.

Once approved, the command is typed at your prompt and appears in the tab as if you had typed it, marked with the same small blue bar in the left margin as the agent's commands in the Agent tab (the margin appears in your tab with the agent's first command there). The agent gets the command's output and exit code when it finishes. A command that is still running after two minutes (a server, say) keeps running; the agent is told so and can read the tab later.

The agent is refused, and nothing is typed, when:

- the tab is shared **Read only**: it is told to ask you to share the tab as Read + run;
- you are typing at the prompt, or a program is running in the tab (`vim`, a Python prompt, a password prompt, a running build): it waits two seconds, then is told what is in the way;
- the tab's shell has no shell integration (anything other than bash or zsh, such as fish): without it the agent can't tell when a command finishes, so it doesn't type into the tab at all;
- the command has more than one line: only single-line commands are typed at your prompt;
- another command of the agent's is already waiting or running in that tab.

tmux panes outside Chatty are only ever read, never run in.

The tmux panes outside Chatty are a separate switch, **Enable Terminal Access** in **Settings → Code Execution** (see [Agents and tools](./agents-and-tools.md)).

## Settings

**Settings → Terminal:**

| Setting | Default | Applies |
|---|---|---|
| Font Family | the theme's code font | at once |
| Font Size | the theme's code font size | at once |
| Shell | your login shell (`$SHELL` on macOS and Linux, PowerShell on Windows) | to new terminals |
| Scrollback Lines | 10,000 | to new terminals |
| Dock Height | 300 pixels | when the dock is next drawn |
| When sharing a terminal | Ask | the next click on a tab's eye icon |

Dragging the dock's top edge updates **Dock Height** too.
