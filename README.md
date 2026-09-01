<p align="center">
  <img src="assets/app_icon/ai-2.png" alt="Chatty" width="128" height="128">
</p>

<h1 align="center">Chatty</h1>

<p align="center">
  <strong>A desktop and terminal AI agent, built in Rust.</strong><br>
  Run multi-tool agents against your chosen LLM provider, with local storage
  and optional fully local workflows via Ollama.
</p>

<p align="center">
  <a href="https://boersmamarcel.github.io/chatty2/">Docs</a> &bull;
  <a href="https://boersmamarcel.github.io/chatty2/user/getting-started.html">Getting started</a> &bull;
  <a href="https://github.com/boersmamarcel/chatty">Marketing</a> &bull;
  <a href="https://github.com/boersmamarcel/chatty2/releases">Releases</a> &bull;
  <a href="AGENTS.md">Agent guide</a>
</p>

---

<p align="center"><img src="assets/animations/hero_high_quality.gif" alt="Chatty in action" width="800"></p>

## What is this repo?

**chatty2** is the product source: the GPUI desktop app (`chatty`), the Ratatui
terminal app (`chatty-tui`), and shared `chatty-core`. Product marketing lives
on [boersmamarcel/chatty](https://github.com/boersmamarcel/chatty) — this README
is a landing page, not the user manual.

| I want to… | Go here |
|---|---|
| Install and start chatting | [Getting started](https://boersmamarcel.github.io/chatty2/user/getting-started.html) |
| Agents, tools, memory, security | [User guides](https://boersmamarcel.github.io/chatty2/) |
| Contribute or extend the code | [AGENTS.md](AGENTS.md) · [CONTRIBUTING.md](CONTRIBUTING.md) |
| Architecture and reference | [docs/INDEX.md](docs/INDEX.md) |

## Download

Grab the latest build from [GitHub Releases](https://github.com/boersmamarcel/chatty2/releases):

| Platform | Format |
|:---------|:-------|
| macOS (Intel & Apple Silicon) | `.dmg` installer |
| Linux (x86_64) | `.tar.gz` archive |
| Windows (x86_64) | `.exe` installer |

On first launch, add a **provider** and a **model** in Settings. Step-by-step
walkthrough (slash commands, workspace, approval modes):
[Getting started](https://boersmamarcel.github.io/chatty2/user/getting-started.html).

## Develop

```bash
make setup        # Linux deps + wasm32-wasip2 (once)
make build
make test-fast    # chatty-core lib tests
make docs-serve   # mdBook at http://localhost:3000
```

Workspace map and conventions: [AGENTS.md](AGENTS.md).
Packaging and CI: [Build & package](https://boersmamarcel.github.io/chatty2/dev/guides/build-package.html).

## License

MIT
