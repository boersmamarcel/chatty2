# Chatting

**When to read this:** You have a model connected and want to know what the chat window can do — rendering, attachments, artifacts, cost tracking, search and themes.

## The composer

- **Model selector** at the bottom switches models mid-conversation; the roster and default come from [Providers & models](./providers-and-models.md).
- Type `/` for the command picker (`↑/↓`, `Enter`). Saved skills appear there with a skill badge. Full list: [slash commands](../dev/reference/slash-commands.md).
- Type `@` to mention a file from the working directory. Hidden files and common build folders (`.git`, `node_modules`, `target`, `dist`, `build`) are skipped.
- **Add attachments** attaches images and PDFs. The buttons only appear for models that accept them.
- The folder icon sets a per-chat working directory: [Agents & tools](./agents-and-tools.md).

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

When browser tools are on, a live view of the page the agent is driving docks in the same panel: [Agents & tools](./agents-and-tools.md).

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

- Conversations are stored in a local database on your machine; there is no hosted sync.
- Titles are generated automatically. The search icon in the title bar opens **Search conversations…**, which filters the sidebar as you type.
- Each conversation's **⋯** menu has **Download**, which saves the transcript as a Markdown file, and **Delete**.
- The sidebar shows the running cost per conversation; each reply shows its input and output tokens and cost. Pricing uses the per-million-token rates on the model, which the OpenRouter catalogue fills in for you.
- **Regenerate** under a reply asks for a fresh answer. Chatty keeps both versions, which is what makes preference-pair export possible ([Advanced](./advanced.md)).

![Token and cost tracking](../assets/animations/advanced_token_tracking.gif)

## Themes and text

Settings → **General** offers twenty-odd theme families (Ayu, Catppuccin, Everforest, Flexoki, Gruvbox, Matrix, Solarized, Tokyo Night and more), a **Dark Mode** switch for the light or dark variant of each, and a **Font Size** from 8 to 32.

## Next

- [Agents & tools](./agents-and-tools.md)
- [Providers & models](./providers-and-models.md)
- [Advanced](./advanced.md)
