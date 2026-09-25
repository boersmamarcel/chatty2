# Chatting

**When to read this:** You have a model connected and want to know what the chat window can do — rendering, attachments, artifacts, cost tracking, search and themes.

## The composer

- **Model selector** at the bottom switches models mid-conversation; the roster and default come from [Providers & models](./providers-and-models.md).
- Type `/` for the command picker (`↑/↓`, `Enter`). Saved skills appear there with a skill badge. Full list: [slash commands](../dev/reference/slash-commands.md).
- Type `@` to mention a file from the working directory. Hidden files and common build folders (`.git`, `node_modules`, `target`, `dist`, `build`) are skipped.
- **Add attachments** attaches images and PDFs. The buttons only appear for models that accept them.
- The folder icon sets a per-chat working directory: [Agents & tools](./agents-and-tools.md).

## Sending while a reply streams

**Send** (or **Enter**) doesn't wait for the current reply to finish — a message sent mid-stream is queued and appears as its own bubble below the reply in progress, with **×** in front to take it back and **↑** to send it now instead, which cancels the current reply and runs that message next. Up to 5 messages can queue at once; a sixth is refused with a notice under the queue. **Stop** cancels the reply but leaves anything queued in place — nothing runs until you send again, and that send runs the queued messages first, in order.

The running indicator above the composer grows a "· 84s · turn 11 · 11254 tok" suffix — elapsed time once a second has passed, then the running turn count and this turn's own token total once a round-trip has completed — a way to see a long reply is still making progress now that replies have no default turn cap.

## Rich rendering

Responses render as Markdown with:

- **Syntax-highlighted code** in dozens of languages, with **Copy code** on every block.

  ![Syntax-highlighted code blocks](../assets/animations/codehighlighting.gif)

- **Math** — inline `$...$` and block `$$...$$`, drawn in the theme colour, with **Copy LaTeX**.

  ![Math rendering](../assets/animations/advanced_math_rendering.gif)

- **Mermaid diagrams** from fenced `mermaid` blocks — flowcharts, sequence, class, ER, Gantt and many more — drawn inline in light or dark to match the theme. **Copy Mermaid** copies the source; **Copy as PNG** copies the image.

  ![Mermaid diagram rendering](../assets/animations/mermaid.gif)

- **Image and PDF previews** for attachments and generated files.

Models that think out loud (for example Claude extended thinking) get their reasoning folded into a collapsible section so it stays inspectable without filling the transcript.

## Artifacts

When the agent produces a document, a card appears in the transcript and the file opens in a panel beside the chat:

- **PDFs** the agent typesets, paged in the panel.

  ![PDF artifact](../assets/animations/artifact_pdf.gif)

- **Charts** — bar, line, pie, donut, area and candlestick — drawn natively and theme-aware, with **Copy as PNG**.

  ![Chart artifact](../assets/animations/artifact_chart.gif)

- **Query results** from SQL the agent ran over CSV, Parquet or JSON files in the workspace, shown as a table with the SQL one tab away.

  ![Table artifact](../assets/animations/artifact_table.gif)

- **Markdown documents** the agent wrote, rendered or as source.

  ![Markdown artifact](../assets/animations/artifact_markdown.gif)

- **PowerPoint decks** the agent writes, rendered as real slide images and paged with Prev/Next, with the extracted text a tab away.

When browser tools are on, a live view of the page the agent is driving docks in the same panel: [Agents & tools](./agents-and-tools.md).

## File explorer

The sidebar doubles as a light IDE over your working directory. A **Chats | Files** toggle sits in the status bar at the bottom of the window, next to the warning/error indicators; switching to Files replaces the conversation list with a tree rooted at your workspace directory (or the current folder if none is set), folders first, and picks up files the agent adds or removes within a couple of seconds without needing a refresh. Switching conversations re-roots the tree to that conversation's own working directory. You can also reach it from the caret next to the artifact button in the titlebar (or the floating button on macOS) — pick **Files**, which expands the sidebar if it's collapsed and switches it to Files mode.

Drag the edge between the sidebar and the chat to resize it — handy for reading long filenames or deeply nested paths in full.

Click a file to open it in the artifact panel, using the same viewers as agent-produced artifacts (Markdown, code, PDF, PowerPoint, images, tables). Right-click a row, the header, or an empty folder for **New file…**, **New folder…**, **Rename…**, **Delete…**, **Reveal in file manager** and **Copy path**. Deleting asks for confirmation first and cannot be undone.

Ctrl/⌘-click a row to add it to a selection, or Shift-click to select a range; the right-click menu then reads **Delete N items…** and **Copy paths** for the whole selection. Drag a selected row onto a folder (or onto the empty area to move it to the workspace root) to move it there — an open file's tab follows it to the new path.

Press **Ctrl/Cmd+P** from anywhere to open **Go to File**, a fuzzy quick-open over every file in the workspace — handy when the sidebar is collapsed or you just want to jump straight to a file. Type to filter, then click or press Enter on a match to open it in the artifact panel.

The Source tab is editable: press **Ctrl/Cmd+S** or click **Save** to write your changes back to disk. An unsaved file shows a dot on its tab; switching tabs keeps your edits, and closing a tab with unsaved changes asks before discarding them.

## Tool-call traces

Every tool call is a collapsible block showing the name, arguments, output, duration and status (success, error or cancelled). File edits get a proper diff — additions green, deletions red, unchanged runs collapsed with *Show N more lines* on large patches. Multi-step work also shows a **To-dos** card that updates in place; see [Agents & tools](./agents-and-tools.md).

## Context window

Long agent runs fill the context quickly. The footer shows:

- **Fill bar** — segments for the system preamble, tool definitions, history and the latest message, coloured green, amber or red as the window fills. It appears once **Max Context Window** is set on the model (**Edit… → Advanced**).
- **Token popover** — hover the bar for per-segment estimates and the provider's input and output counts.
- **`/compact`** — summarises the older half of the history so the run can continue.

## Pull request status

When the workspace is a git checkout whose `origin` is on GitHub, a bar above the composer shows the pull request for the current branch: number, repository, branch, `+added −deleted` and a CI pill listing each check. Click the number, repository or branch to open the PR in your browser; `×` hides the bar until the branch or PR changes.

![Pull request status bar](../assets/animations/pr_status_bar.gif)

The bar follows **Enable Git Integration** in Settings → Code Execution. Private repositories need the GitHub CLI (`gh`) installed and signed in; public ones work without it. No PR, no GitHub remote or no workspace means no bar. The terminal app shows the same thing (`#591 open ✓`) in its status line.

## Conversations, cost and search

- Conversations are stored in a local database on your machine by default.
- Titles are generated automatically. The search icon in the title bar opens **Search conversations…**, which filters the sidebar as you type.
- Each conversation's **⋯** menu has **Download**, which saves the transcript as a Markdown file, **Take online…**, and **Delete**.
- The sidebar shows the running cost per conversation; each reply shows its input and output tokens and cost. Pricing uses the per-million-token rates on the model, which the OpenRouter catalogue fills in for you.
- Chatty only ever appends to the prompt it sends — the system instructions, tool list and prior turns already sent are never rewritten — so a provider's prompt cache keeps recognising them turn after turn. Providers that price cached input lower pass that saving straight through to the cost shown here.
- **Regenerate** under a reply asks for a fresh answer. Chatty keeps both versions, which is what makes preference-pair export possible ([Advanced](./advanced.md)).

![Token and cost tracking](../assets/animations/advanced_token_tracking.gif)

## Taking a conversation online

**Take online…** in a conversation's **⋯** menu uploads its history to a `chatty-server` you name and continues it there — useful for a long-running task you want to keep going after you close the laptop. A globe icon badges the conversation in the sidebar while it runs remotely, and the same menu offers **Bring back here** to return it. The confirmation dialog lists exactly what moves (message history, traces, the model id) and what never does (workspace files, attachments, MCP servers, memory, skills, and provider API keys — those stay on this machine). The conversation's local copy is kept either way, so bringing it back does not lose anything, and a move mid-turn is refused until the turn finishes.

## Themes and text

Settings → **General** offers twenty-odd theme families (Ayu, Catppuccin, Everforest, Flexoki, Gruvbox, Matrix, Solarized, Tokyo Night and more), a **Dark Mode** switch for the light or dark variant of each, and a **Font Size** from 8 to 32.

## Next

- [Agents & tools](./agents-and-tools.md)
- [Providers & models](./providers-and-models.md)
- [Advanced](./advanced.md)
