# chatty-module-sdk

SDK for building [chatty](https://github.com/boersmamarcel/chatty2) WASM agent modules.

Compile your module to `wasm32-wasip2` and run it inside chatty's sandboxed WASM runtime — exposed via OpenAI, MCP, and A2A protocols simultaneously.

## Quick start

```bash
# Prerequisites
rustup target add wasm32-wasip2

# Scaffold a new module
cargo generate --path templates/module --name my-agent

# Or add the SDK to an existing crate
cargo add chatty-module-sdk
```

## Usage

```rust
use chatty_module_sdk::*;

#[derive(Default)]
struct MyAgent;

impl ModuleExports for MyAgent {
    fn chat(&self, req: ChatRequest) -> Result<ChatResponse, String> {
        let resp = llm::complete("claude-sonnet-4-20250514", &req.messages, None)?;
        Ok(ChatResponse {
            content: resp.content,
            tool_calls: vec![],
            usage: resp.usage,
        })
    }

    fn invoke_tool(&self, _name: String, _args: String) -> Result<String, String> {
        Err("no tools".into())
    }

    fn list_tools(&self) -> Vec<ToolDefinition> {
        vec![]
    }

    fn get_agent_card(&self) -> AgentCard {
        AgentCard {
            name: "my-agent".into(),
            display_name: "My Agent".into(),
            description: "A demo agent".into(),
            version: "0.1.0".into(),
            skills: vec![],
            tools: vec![],
        }
    }
}

export_module!(MyAgent);
```

## Host imports

Modules run in a sandbox and access host capabilities through typed imports:

| Module | Function | Description |
|--------|----------|-------------|
| `llm` | `complete(model, messages, tools)` | Run LLM completion via host-managed provider |
| `config` | `get(key)` | Read configuration from module manifest |
| `log` | `info(msg)`, `debug(msg)`, `warn(msg)`, `error(msg)` | Structured logging |

## License

MIT
