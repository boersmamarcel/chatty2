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

The loop also watches itself while it runs, so a bad turn recovers instead of quietly failing: a model that's still reasoning is not mistaken for a hung one, a turn that comes back with nothing is retried rather than treated as done, and a model stuck re-issuing the same tool call is nudged onto a different one. A tool's own error message reaches the transcript intact, so a failure is reported as a failure rather than read back as a success.

For multi-step work the agent writes a **plan** first: a goal and an ordered to-do list. A **To-dos** card appears in the transcript and updates in place (pending, in progress, done, blocked) with a progress counter. Scroll past it and a compact **Plan N of M** strip pins to the top of the transcript — click to unfold the plan, or press `Esc` to dismiss the overlay without cancelling the run. The agent ticks steps off as it goes and runs a verification step before its final reply. In the terminal interface the card renders inline in the transcript instead, and its position also appears in the status bar — see [Terminal interface](./terminal.md).

The agent can also pause mid-turn to ask up to 4 clarifying questions, each with pre-made options plus a free-text answer (`ask_user`). It shows up as a card above the chat input; pick an option or type your own answer, then submit. Questions you leave blank are sent as unanswered so the agent knows what it still doesn't know. In the terminal interface it replaces the input row instead — see [Terminal interface](./terminal.md).

## Enable tools

Everything below is off until you switch it on in **Settings → Code Execution**:

1. Set a **Workspace Directory** (an absolute path). File, shell and git tools can only touch files inside it.
2. Turn on **Enable Code Execution**.
3. Choose an **Approval Mode** — what they mean is on [Security & sandboxing](./security.md).

Optional switches on the same page: **Enable Git Integration** (git tools and the pull request bar), **Enable Browser Tools** with **Allow Browser Access to Private Network**, **Enable Code Execution Tool** with **Enable Docker Fallback** and a **Docker Host** field, plus **Max Agent Turns**, a command **Timeout**, a **Max Output** size and **Network Isolation**. Web access has its own page, **Settings → Internet**, and is on by default.

**Per-chat working directory.** The folder icon in the composer (**Select Working Directory**) overrides the workspace for that conversation; `×` resets it to the global one. The override is saved with the conversation. `/cd <dir>` does the same from the keyboard, and `/add-dir <dir>` widens the workspace by one more directory.

## What the agent can do

Tool names and their exact scopes are in the [tools catalog](../dev/reference/tools-catalog.md); this is the map.

### Files and code

The agent can read files, list directories, search by glob or content, jump to a definition, and create, edit, rename, move or delete files — always inside the workspace. Edits show as diffs in the transcript. With git integration on it can also inspect status and diffs, stage, commit and merge branches.

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

**Better keyless results with a local reranker.** Without a search API key, you can have the results reordered by a small cross-encoder model on your own machine. Fill in **Rerank Endpoint** and **Rerank Model** under **Settings → Internet → Keyless search**, then press **Test**. In our evaluation this took the right page in the top five from 48 % to 57 % of questions, and the right page at the very top from 27 % to 49 %. The cost is about one second per search (p95 went from ~2.3 s to ~3.2 s). If the endpoint is down, search keeps working and returns results in their normal order. The reranker is not used when a Tavily or Brave key is set.

Any server with a Cohere/Jina-style `/rerank` endpoint works. For `BAAI/bge-reranker-v2-m3` (about 1.1 GB of GPU memory at fp16, or ~600 MB as a GGUF on CPU), use one of:

```bash
# vLLM (GPU); vLLM recognises the model as a reranker by itself
vllm serve BAAI/bge-reranker-v2-m3 --port 8001 --max-model-len 1024
# endpoint: http://127.0.0.1:8001/rerank   model: BAAI/bge-reranker-v2-m3

# llama.cpp (CPU or GPU), with a GGUF build of the same model
llama-server -m bge-reranker-v2-m3-Q8_0.gguf --reranking --port 8001
# endpoint: http://127.0.0.1:8001/v1/rerank   model: bge-reranker-v2-m3
```

![Web fetch](../assets/animations/webfetch.gif)

The browser tools drive a real Chrome on your machine, so the agent can look at what it just built instead of guessing: it renders the page, screenshots it, spots the problem, fixes it, and re-checks. It can also click — a button, a link, a menu item — and fill in text fields, by picking an element out of its own snapshot of the page rather than guessing coordinates or positions; the click or typing is refused if the element moved, is covered by something else, or is off screen. Typing replaces a field's contents and never submits it — that's a separate click the agent makes on its own. The agent never types into a password or card-number field; those are always refused, so sign in yourself after taking control. While it works, a live view of the page docks beside the chat; you can click and type in it, and take control at any moment. **Hand back & continue** gives the browser back to the agent and tells it to take a fresh look at the page and carry on from there.

One limit worth knowing: the screenshot reaches the model on its **next** turn, not inside the tool result. In practice the agent captures the screenshot, finishes its turn, and reviews the image on the turn after. Say *keep going* if it stops after capturing.

A link that opens a new tab or window — `target="_blank"`, `window.open`, a "Sign in with…" popup — is followed automatically: the live view, your clicks and typing, and the agent's own tools all switch to the new tab. Open tabs sit in the artifact panel's tab bar next to the open files, each with a globe icon and a × to close it, so you can click back to the page underneath (or to a file) while the popup is still open. Switching to a file leaves the browser tabs in the bar; clicking one brings the page back. Closing the tab you are on shows the one next to it, and closing the last one leaves a blank page. Only the tab you are looking at is streamed live. The same address filtering applies to whatever any tab navigates to, on its own or otherwise: a tab that lands somewhere the browser is not allowed to reach is marked blocked in the bar and shows nothing until it comes back somewhere allowed.

By default the browser is limited to `localhost` and files inside your workspace — it is for reviewing your own work. With Internet access on it can also open public websites, with the same address filtering as fetch and search so private and internal network targets stay out of reach. If you need it to reach something on your own network — another machine on your LAN, say — turn on **Allow Browser Access to Private Network**; cloud-metadata addresses (`169.254.x.x`) stay blocked either way. The browser profile is never signed in to anything, so looking at a page never asks for approval. Clicking and typing are the exception: on `localhost` and workspace files they're automatic, but a click or a keystroke on any other site asks you to approve it first, every time — the agent may be acting inside a session you're logged into under take-control, and that always gets a confirmation. Turn the tools on with **Enable Browser Tools**; they need a workspace, where screenshots and console logs are kept.

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
