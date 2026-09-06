# Chatty

<div class="home-hero">
<p class="lede">A desktop and terminal AI agent, built in Rust. Run multi-tool agents against the LLM provider you choose, with everything stored locally and an optional fully local setup through Ollama.</p>
</div>

<div class="hero-links">
  <a href="user/getting-started.md">Get started</a>
  <a href="https://github.com/boersmamarcel/chatty2/releases" class="secondary">Download</a>
  <a href="https://github.com/boersmamarcel/chatty2" class="secondary">Source on GitHub</a>
</div>

![Chatty in action](./assets/animations/hero.gif)

<div class="cards">
  <a class="card" href="user/getting-started.md"><span class="eyebrow">Use Chatty</span><strong>Getting started</strong><span>Install, connect a provider, add a model, send your first message.</span></a>
  <a class="card" href="user/agents-and-tools.md"><span class="eyebrow">Use Chatty</span><strong>Agents &amp; tools</strong><span>Let the agent read and edit files, run code, query data and browse.</span></a>
  <a class="card" href="dev/guides/build-wasm-module.md"><span class="eyebrow">Extend it</span><strong>Build a WASM plugin</strong><span>Ship your own agent module and publish it to the Hive marketplace.</span></a>
  <a class="card" href="dev/start/build-and-run.md"><span class="eyebrow">Contribute</span><strong>Build and run in 10 minutes</strong><span>Clone, build, run the terminal app against a local model, run the tests.</span></a>
</div>

## Why Chatty

**Your provider, your data.** Connect OpenRouter, Azure OpenAI or a local Ollama instance. Conversations, memory and settings stay in local files and a local SQLite database; nothing is hosted by Chatty.

**An agent, not a chat box.** The model can plan, call tools, read and edit files in a workspace you choose, run code in a sandbox, query spreadsheets and data files, search the web, and drive a real browser to check its own work. Every side-effecting action goes through an approval mode you control.

**Two front ends, one engine.** The desktop app and the terminal app share the same core, so the agent behaves the same whether you are chatting in a window or scripting `chatty-tui` headless in a pipeline. Sub-agents are just headless terminal instances.

**Fast and native.** Built on GPUI, the GPU-accelerated UI framework from the Zed editor. Markdown, code, LaTeX math, Mermaid diagrams, charts, PDFs and tables render natively in the transcript.

## Download

| Platform | Format |
|----------|--------|
| macOS (Intel & Apple Silicon) | `.dmg` installer |
| Linux (x86_64) | `.tar.gz` archive or AppImage |
| Windows (x86_64) | `.exe` installer |

All builds are on [GitHub Releases](https://github.com/boersmamarcel/chatty2/releases). The app checks for updates in the background and verifies them before installing.

## Where to go next

| I want to… | Read |
|------------|------|
| Install and start chatting | [Getting started](./user/getting-started.md) |
| Understand what the agent can do | [Agents & tools](./user/agents-and-tools.md) · [Security & approvals](./user/security.md) |
| Use the terminal app or sub-agents | [Terminal interface](./user/terminal.md) · [Sub-agents](./user/sub-agents.md) |
| Write a plugin or an MCP integration | [Build a WASM plugin](./dev/guides/build-wasm-module.md) · [Extensions & MCP](./user/extensions.md) |
| Contribute to the code | [Build and run](./dev/start/build-and-run.md) · [Contributing patterns](./dev/contributing-patterns.md) |
| Look something up | [Reference](./dev/crates.md) · [Glossary](./dev/glossary.md) |
