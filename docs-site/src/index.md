# Chatty documentation

[Chatty](https://github.com/boersmamarcel/chatty2) is a Rust desktop and
terminal AI agent. This site is the **user + developer** manual. Product
marketing lives on [boersmamarcel/chatty](https://github.com/boersmamarcel/chatty).

<div class="hero-links">
  <a href="user/getting-started.md">Getting started</a>
  <a href="dev/architecture/system-overview.md" class="secondary">System overview</a>
  <a href="dev/agents.md" class="secondary">Agent guide</a>
  <a href="https://github.com/boersmamarcel/chatty" class="secondary">Marketing site</a>
</div>

## Who is this for?

| Audience | Start here |
|----------|------------|
| **End users** | [Getting started](./user/getting-started.md) · [Agents](./user/agents.md) · [Features](./user/features.md) |
| **Coding agents** | [Agent quick-start](./dev/agents.md) · [Doc index](./dev/doc-index.md) · [Component map](./dev/architecture/component-map.md) |
| **Contributors** | [System overview](./dev/architecture/system-overview.md) · [Contributing patterns](./dev/contributing-patterns.md) |
| **Research → product** | [App ↔ research bridge](./dev/adrs/app-research-bridge.md) · [Paper pipeline](./dev/adrs/paper-to-product-pipeline.md) · [Modules](./dev/research/modules/index.md) |

## Documentation map

```mermaid
flowchart LR
  Home[index.md] --> Sys[system-overview]
  Home --> Map[component-map]
  Home --> Agents[AGENTS.md]
  Sys --> Arch[architecture docs]
  Map --> Ref[reference pages]
  Agents --> Guides[how-to guides]
```

## Single source of truth

Architecture pages live in [`docs/`](https://github.com/boersmamarcel/chatty2/tree/main/docs) in the repo.
This site is **built** from those files — edit the repo markdown, not copies here.

```bash
make docs-gen   # regenerate reference tables
make docs       # sync + build
make docs-serve # local preview at http://localhost:3000
```
