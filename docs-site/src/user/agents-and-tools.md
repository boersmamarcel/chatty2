# Agents & tools

**When to read this:** You want to understand how the agent works, switch its tools on, and see what it can do once they are.

## How the loop works

Each message builds an agent from your enabled tools and extensions, then runs a streaming multi-step loop:

```
You send a message
       │
       ▼
  Agent reasons → decides to call a tool
       │
       ▼
  Tool runs (with your approval if required)
       │
       ▼
  Agent reads the result → reasons again
       │        ...repeats up to Max Agent Turns (default 10)...
       ▼
  Agent streams its final answer to you
```

Tool calls, inputs, outputs and reasoning render as collapsible trace blocks beside the reply ([Chatting](./chatting.md)).

For multi-step work the agent writes a **plan** first: a goal and an ordered to-do list. A **To-dos** card appears in the transcript and updates in place (pending, in progress, done, blocked) with a progress counter. Scroll past it and a compact **Plan N of M** strip pins to the top of the transcript — click to unfold the plan, or press `Esc` to dismiss the overlay without cancelling the run. The agent ticks steps off as it goes and runs a verification step before its final reply.

The agent can also pause mid-turn to ask up to 4 clarifying questions, each with pre-made options plus a free-text answer (`ask_user`). It shows up as a card above the chat input; pick an option or type your own answer, then submit. Questions you leave blank are sent as unanswered so the agent knows what it still doesn't know. In the terminal interface it replaces the input row instead — see [Terminal interface](./terminal.md).

## Enable tools

Everything below is off until you switch it on in **Settings → Code Execution**:

1. Set a **Workspace Directory** (an absolute path). File, shell and git tools can only touch files inside it.
2. Turn on **Enable Code Execution**.
3. Choose an **Approval Mode** — what they mean is on [Security & sandboxing](./security.md).

Optional switches on the same page: **Enable Git Integration** (git tools and the pull request bar), **Enable Browser Tools**, **Enable Code Execution Tool** with **Enable Docker Fallback** and a **Docker Host** field, plus **Max Agent Turns**, a command **Timeout**, a **Max Output** size and **Network Isolation**. Web access has its own page, **Settings → Internet**, and is on by default.

**Per-chat working directory.** The folder icon in the composer (**Select Working Directory**) overrides the workspace for that conversation; `×` resets it to the global one. The override is saved with the conversation. `/cd <dir>` does the same from the keyboard, and `/add-dir <dir>` widens the workspace by one more directory.

## What the agent can do

Tool names and their exact scopes are in the [tools catalog](../dev/reference/tools-catalog.md); this is the map.

### Files and code

The agent can read files, list directories, search by glob or content, jump to a definition, and create, edit, rename, move or delete files — always inside the workspace. Edits show as diffs in the transcript. With git integration on it can also inspect status and diffs, stage and commit.

*Rename the `User` struct to `Account` across the project and update every import.*

Reads never ask. Writes, deletes and git changes follow your approval mode; inside the workspace they apply immediately under either auto-approve mode.

### Shell and running code

Shell commands run in a persistent session inside the sandbox, with streaming output, the workspace as its working directory and your [secrets](./security.md) available as environment variables. **Enable Code Execution Tool** adds a code runner: simple Python runs on the host in milliseconds; with **Enable Docker Fallback** (Docker must be running) the agent can also run JavaScript, TypeScript, Rust, Bash and Python that needs third-party packages, each in a fresh container. Chatty finds the Docker socket on its own, including rootless Docker and Docker Desktop; **Docker Host** covers unusual setups (for example `/run/user/1000/docker.sock`).

*Run the test suite, fix whatever fails, and run it again.*

Shell commands and code runs follow your approval mode; sandboxed commands run without prompting under **Auto-approve Sandboxed**.

### Data and documents

The agent can run SQL over CSV, Parquet and JSON files in the workspace and describe their schema, read and write Excel, Word and PowerPoint files, extract text and page images from PDFs, typeset PDFs and draw charts. Results open as [artifacts](./chatting.md) beside the chat.

*Load `sales.parquet`, chart revenue by region for the last four quarters, and write a two-page PDF summary.*

Reading and querying never ask. Writing spreadsheets and other files follows your approval mode.

### Web and the built-in browser

With **Settings → Internet** on, the agent can search the web (Tavily or Brave if you add a key, a basic fallback otherwise) and fetch any public page as readable text. The same page also holds optional cloud services — a hosted browser agent and a cloud code sandbox — which need their own API keys and stay off until you add one.

![Web fetch](../assets/animations/webfetch.gif)

The browser tools drive a real Chrome on your machine, so the agent can look at what it just built instead of guessing: it renders the page, screenshots it, spots the problem, fixes it, and re-checks. While it works, a live view of the page docks beside the chat; you can click and type in it, and take control at any moment.

One limit worth knowing: the screenshot reaches the model on its **next** turn, not inside the tool result. In practice the agent captures the screenshot, finishes its turn, and reviews the image on the turn after. Say *keep going* if it stops after capturing.

By default the browser is limited to `localhost` and files inside your workspace — it is for reviewing your own work. With Internet access on it can also open public websites, with the same address filtering as fetch and search so private and internal network targets stay out of reach. The browser profile is never signed in to anything, so none of these tools asks for approval either way. Turn them on with **Enable Browser Tools**; they need a workspace, where screenshots and console logs are kept.

Chrome is not bundled. If you already have Chrome, Chromium or Edge installed, that is used; otherwise the first browser call downloads a pinned Chrome for Testing build — roughly 190 MB, once — and verifies it before use. Expect that first call to take a minute.

*Open http://localhost:3000, screenshot the checkout page at 375 px wide, and fix anything that overflows.*

![Internet access settings](../assets/animations/advanced_internet_access_settings.gif)

### Memory, skills, sub-agents and planning

The agent can remember facts and preferences across conversations and recall them later, save a procedure it just worked out as a named skill and load it again ([Memory & skills](./memory-and-skills.md)), hand independent subtasks to headless [sub-agents](./sub-agents.md), call agents you have installed as extensions, list its own tools and MCP servers, and keep the to-do plan described above.

*Remember that this project uses pnpm, then split the migration into three sub-agents, one per package.*

Memory, skills and planning never ask. Sub-agents run their own tool calls without prompting only under **Auto-approve All**.

## Extensions

MCP servers, agents and modules from the Hive marketplace, the built-in catalog (Hugging Face, Notion, Atlassian, Google) and your own servers plug in under **Settings → Extensions**: [Extensions](./extensions.md).

## Next

- [Security & sandboxing](./security.md)
- [Extensions](./extensions.md)
- [Memory & skills](./memory-and-skills.md)
