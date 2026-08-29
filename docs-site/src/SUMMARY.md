# Summary

[Introduction](./index.md)

---

# User guides

- [Getting started](./user/getting-started.md)
- [Providers & models](./user/providers-and-models.md)
- [Agentic tools](./user/agentic-tools.md)

---

# Developer guide

- [Agent quick-start (AGENTS.md)](./dev/agents.md)
- [Contributing patterns (CLAUDE.md)](./dev/contributing-patterns.md)
- [Documentation index](./dev/doc-index.md)
- [Where do I…? (decision tree)](./dev/where-to-look.md)

---

# Architecture

- [System overview](./dev/architecture/system-overview.md)
- [Component map (diagrams)](./dev/architecture/component-map.md)
- [Architecture overview](./dev/architecture/architecture-overview.md)
- [Workspace crate split](./dev/architecture/workspace-crate-split.md)
- [Entity communication](./dev/architecture/entity-communication.md)
- [Stream manager](./dev/architecture/stream-manager.md)
- [Rendering system](./dev/architecture/rendering-system.md)
- [Token tracking](./dev/architecture/token-tracking.md)
- [Agent memory](./dev/architecture/agent-memory.md)

# Modules & extensions

- [A2A and WASM modules](./dev/architecture/a2a-and-wasm-modules.md)
- [WIT reference](./dev/architecture/wit-reference.md)
- [Curated MCP catalog](./dev/architecture/curated-mcp-catalog.md)
- [Pre-built APIs](./dev/architecture/pre-built-apis.md)

# Operations

- [Release process](./dev/architecture/RELEASE_PROCESS.md)
- [Monty sandbox](./dev/architecture/monty-sandbox.md)
- [Debug UI](./dev/architecture/debug_ui.md)
- [Refactor follow-ups](./dev/architecture/refactor-followups.md)

# Crates

- [chatty-core](./dev/crates/chatty-core.md)
- [chatty-gpui](./dev/crates/chatty-gpui.md)
- [chatty-tui](./dev/crates/chatty-tui.md)
- [chatty-wasm-runtime](./dev/crates/chatty-wasm-runtime.md)
- [chatty-module-registry](./dev/crates/chatty-module-registry.md)
- [chatty-protocol-gateway](./dev/crates/chatty-protocol-gateway.md)
- [chatty-module-sdk](./dev/crates/chatty-module-sdk.md)
- [chatty-trace](./dev/crates/chatty-trace.md)
- [chatty-playbook](./dev/crates/chatty-playbook.md)
- [chatty-flow](./dev/crates/chatty-flow.md)
- [chatty-optimize](./dev/crates/chatty-optimize.md)
- [hive-client](./dev/crates/hive-client.md)
- [hive-billing-sdk](./dev/crates/hive-billing-sdk.md)

# Research pipeline

- [App ↔ research bridge](./dev/adrs/app-research-bridge.md)
- [Paper → experiment → product](./dev/adrs/paper-to-product-pipeline.md)
- [Experiment protocol](./dev/adrs/experiment-protocol.md)
- [Promotion log](./dev/adrs/promotion-log.md)
- [Settings integration map](./dev/adrs/settings-integration-map.md)

## Modules (M0–M4)

- [Overview](./dev/research/modules/index.md)
- [M0 Trace contract](./dev/research/modules/m0-trace.md)
- [M1 ReAct](./dev/research/modules/m1-react.md)
- [M2 AFlow](./dev/research/modules/m2-aflow.md)
- [M3 GEPA](./dev/research/modules/m3-gepa.md)
- [M4 ACE](./dev/research/modules/m4-ace.md)

# ADRs & research

- [Harbor pivot](./dev/adrs/harbor-pivot.md)
- [chatty-trace promises](./dev/adrs/crate-promises-chatty-trace.md)
- [chatty-playbook promises](./dev/adrs/crate-promises-chatty-playbook.md)
- [chatty-flow promises](./dev/adrs/crate-promises-chatty-flow.md)
- [Cost model](./dev/adrs/cost-model.md)
- [AppWorld decision](./dev/adrs/appworld-decision.md)

---

# Reference

- [Tools catalog](./dev/reference/tools-catalog.md)
- [Slash commands](./dev/reference/slash-commands.md)
- [CLI flags (chatty-tui)](./dev/reference/cli-flags.md)
- [Environment variables](./dev/reference/env-vars.md)
- [GPUI event catalog](./dev/reference/event-catalog.md)
- [Singleton inventory](./dev/reference/singleton-inventory.md)

# How-to guides

- [Add a new LLM provider](./dev/guides/add-provider.md)
- [Add a new LLM tool](./dev/guides/add-tool.md)
- [Debug streams & rendering](./dev/guides/debug-streams.md)
- [Build & package](./dev/guides/build-package.md)
