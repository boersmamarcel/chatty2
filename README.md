<p align="center">
  <img src="assets/app_icon/ai-2.png" alt="Chatty" width="128" height="128">
</p>

<h1 align="center">Chatty</h1>

<p align="center">
  <strong>A desktop and terminal AI agent built in Rust.</strong>
  Run multi-tool agents against your chosen LLM provider, with local storage
  and optional fully local workflows via Ollama.
</p>

<p align="center">
  <a href="https://github.com/boersmamarcel/chatty">Marketing</a> &bull;
  <a href="https://boersmamarcel.github.io/chatty2/">Docs</a> &bull;
  <a href="https://github.com/boersmamarcel/chatty2/releases">Releases</a> &bull;
  <a href="#development">Development</a>
</p>

---

<p align="center"><img src="assets/animations/hero_high_quality.gif" alt="Chatty in action" width="800"></p>

## What this repo is

**chatty2** is the source for the Chatty desktop app (`chatty`) and terminal
agent (`chatty-tui`). Product marketing lives on
[boersmamarcel/chatty](https://github.com/boersmamarcel/chatty). User guides and
developer docs are published on
[GitHub Pages](https://boersmamarcel.github.io/chatty2/).

| I want to… | Go here |
|---|---|
| See the product / demos | [Marketing site](https://github.com/boersmamarcel/chatty) |
| Install and use Chatty | [Getting started](https://boersmamarcel.github.io/chatty2/user/getting-started.html) |
| Understand the codebase | [Developer docs](https://boersmamarcel.github.io/chatty2/) · [AGENTS.md](AGENTS.md) |
| Download a build | [GitHub Releases](https://github.com/boersmamarcel/chatty2/releases) |

## Download

| Platform | Format |
|:---------|:-------|
| macOS (Intel & Apple Silicon) | `.dmg` installer |
| Linux (x86_64) | `.tar.gz` archive |
| Windows (x86_64) | `.exe` installer |

On first launch, add a provider and a model in **Settings**, then send a
message. Walkthrough: [Getting started](https://boersmamarcel.github.io/chatty2/user/getting-started.html).

## User guides

- [Why Chatty?](https://boersmamarcel.github.io/chatty2/user/overview.html)
- [Agents](https://boersmamarcel.github.io/chatty2/user/agents.html)
- [Providers & models](https://boersmamarcel.github.io/chatty2/user/providers-and-models.html)
- [Agentic tools](https://boersmamarcel.github.io/chatty2/user/agentic-tools.html)
- [Memory & skills](https://boersmamarcel.github.io/chatty2/user/memory-and-skills.html)
- [Sub-agents](https://boersmamarcel.github.io/chatty2/user/sub-agents.html)
- [Security](https://boersmamarcel.github.io/chatty2/user/security.html)
- [Features](https://boersmamarcel.github.io/chatty2/user/features.html)
- [Terminal (`chatty-tui`)](https://boersmamarcel.github.io/chatty2/user/terminal.html)

## Development

```bash
make setup         # Linux deps + wasm32-wasip2 (once)
make build         # cargo build
make test-fast     # chatty-core lib tests
make docs-serve    # mdBook at http://localhost:3000
cargo run -p chatty-gpui   # desktop
cargo run -p chatty-tui    # terminal
```

Workspace map and conventions: [AGENTS.md](AGENTS.md).
PR workflow: [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT
