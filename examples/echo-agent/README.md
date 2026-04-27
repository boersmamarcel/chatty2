# Echo Agent — Example Chatty Module

A simple but complete WASM module demonstrating the chatty module SDK.

## Features

- **Chat** — delegates to the host LLM with a system prompt and agentic tool loop
- **Tool** — `transform_text` supports: uppercase, lowercase, reverse, word_count, char_count, title_case, snake_case
- **Agent card** — proper metadata for discovery via `list_agents` and invocation via `invoke_agent`

## Build

```bash
# Ensure the wasm32-wasip2 target is installed
rustup target add wasm32-wasip2

# Build the WASM component
cd examples/echo-agent
cargo build --release
```

The output is at `target/wasm32-wasip2/release/echo_agent.wasm`.

## Install locally

Copy to the chatty modules directory:

```bash
mkdir -p ~/.local/share/chatty/modules/echo-agent
cp target/wasm32-wasip2/release/echo_agent.wasm ~/.local/share/chatty/modules/echo-agent/
```

Create `~/.local/share/chatty/modules/echo-agent/module.toml`:

```toml
[module]
name = "echo-agent"
version = "0.1.0"
description = "A text processing agent"
wasm = "echo_agent.wasm"

[capabilities]
tools = ["transform_text"]
chat = true
agent = true

[protocols]
a2a = true
```

## Publish to Hive

```bash
# From the echo-agent directory (with manifest.toml present)
# Use the chatty integration test, curl, or the publish_wasm_module tool:
curl -X POST http://localhost:8080/api/v1/modules \
  -H "Authorization: Bearer $TOKEN" \
  -F "manifest=@manifest.toml" \
  -F "wasm=@target/wasm32-wasip2/release/echo_agent.wasm"
```

## Usage in Chatty

After installing (from marketplace or manually):

```
Use the echo-agent to transform "hello world" to uppercase
```

Or via the `/agent` slash command:

```
/agent echo-agent Reverse the text "Hello World"
```
