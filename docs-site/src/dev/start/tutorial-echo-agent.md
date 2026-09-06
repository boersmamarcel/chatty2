# Tutorial: echo-agent

**When to read this:** Your first WASM plugin — echo messages, expose tools,
and optionally delegate to the host LLM.

**Full source:**
[`modules/echo-agent/`](https://github.com/boersmamarcel/chatty2/tree/main/modules/echo-agent)
(~150 lines). Also see
[`modules/echo-agent/README.md`](https://github.com/boersmamarcel/chatty2/blob/main/modules/echo-agent/README.md)
for build commands and integration-test checklist.

## What it demonstrates

| Feature | Behaviour |
|---------|-----------|
| **chat** | Prefixes the last user message with `Echo: `. If the message contains `"use llm"`, calls the host LLM instead. |
| **tools** | `echo`, `reverse`, `count_words` — callable via MCP or from another agent |
| **logging** | `log::info` / `debug` / `warn` / `error` forwarded to the host |
| **agent card** | Metadata for discovery (`list_agents`, `/.well-known/agent.json`) |

## Core: the `ModuleExports` trait

Every plugin implements four methods and wires them with `export_module!`:

```rust
use chatty_module_sdk::{
    export_module, AgentCard, ChatRequest, ChatResponse, ModuleExports,
    Role, Skill, ToolDefinition,
};

#[derive(Default)]
pub struct EchoAgent;

impl ModuleExports for EchoAgent {
    fn chat(&self, req: ChatRequest) -> Result<ChatResponse, String> { /* … */ }
    fn invoke_tool(&self, name: String, args: String) -> Result<String, String> { /* … */ }
    fn list_tools(&self) -> Vec<ToolDefinition> { /* … */ }
    fn get_agent_card(&self) -> AgentCard { /* … */ }
}

export_module!(EchoAgent);
```

## Step 1 — Echo in `chat`

Find the last user message and return a prefixed reply:

```rust
fn chat(&self, req: ChatRequest) -> Result<ChatResponse, String> {
    chatty_module_sdk::log::info("echo-agent: handling chat request");

    let last_user_msg = req
        .messages
        .iter()
        .rfind(|m| m.role == Role::User)
        .map(|m| m.content.as_str())
        .unwrap_or("");

    Ok(ChatResponse {
        content: format!("Echo: {last_user_msg}"),
        tool_calls: vec![],
        usage: None,
    })
}
```

[Full implementation → `src/lib.rs`](https://github.com/boersmamarcel/chatty2/blob/main/modules/echo-agent/src/lib.rs)

## Step 2 — Optional host LLM delegation

When the user message contains `"use llm"`, call the host import instead of
echoing locally. The host holds API keys; your module passes conversation
history through:

```rust
let content = if last_user_msg.contains("use llm") {
    chatty_module_sdk::log::debug("echo-agent: delegating to host LLM");
    match chatty_module_sdk::llm::complete("", &req.messages, None) {
        Ok(resp) => resp.content,
        Err(e) => format!("LLM error: {e}"),
    }
} else {
    format!("Echo: {last_user_msg}")
};
```

Try in Chatty after loading the module:

```text
/agent echo-agent use llm to explain what WASM component model is in one sentence
```

## Step 3 — Define tools

`list_tools` advertises JSON-schema parameters. `invoke_tool` executes them
locally (pure Rust, no network):

```rust
fn invoke_tool(&self, name: String, args: String) -> Result<String, String> {
    match name.as_str() {
        "echo" => Ok(args),
        "reverse" => Ok(args.chars().rev().collect()),
        "count_words" => Ok(args.split_whitespace().count().to_string()),
        _ => Err(format!("unknown tool: {name}")),
    }
}
```

Tools are exposed on the gateway at `POST /mcp/echo-agent` (`tools/list`,
`tools/call`) without running the full chat path.

## Step 4 — Agent card

`get_agent_card` returns discovery metadata. Keep `name` aligned with
`module.toml` `[module].name`:

```rust
fn get_agent_card(&self) -> AgentCard {
    AgentCard {
        name: "echo-agent".into(),
        display_name: "Echo Agent".into(),
        description: "Reference echo agent demonstrating all chatty SDK features.".into(),
        version: "0.1.0".into(),
        skills: vec![Skill {
            name: "echoing".into(),
            description: "Echoes user messages; optionally calls the host LLM.".into(),
            examples: vec![
                "Say hello".into(),
                "use llm to answer: what is 2+2?".into(),
            ],
        }],
        tools: vec![],
    }
}
```

## Build and load

```sh
cd modules/echo-agent
cargo build --target wasm32-wasip2 --release
cp target/wasm32-wasip2/release/echo_agent.wasm .

# Linux: install into Chatty modules dir
cp -r . ~/.local/share/chatty/modules/echo-agent/
```

Enable **Settings → Modules**, then verify:

```sh
# MCP tools
curl -s -X POST http://localhost:8420/mcp/echo-agent \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}'

# A2A chat
curl -s -X POST http://localhost:8420/a2a/echo-agent \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"message/send",
       "params":{"message":{"parts":[{"type":"text","text":"hello"}]}}}'
```

## Automated tests

From the repo root:

```sh
make wasm-modules
cargo test -p chatty-protocol-gateway echo_agent
```

The suite covers registry load, tool invocation, chat echo, agent card, and
all three gateway protocols.

## Next steps

- Scaffold your own module: [`templates/module/`](https://github.com/boersmamarcel/chatty2/tree/main/templates/module)
- Multi-turn LLM + tools loop: [Tutorial: benford-agent](./tutorial-benford-agent.md)
- Architecture and gateway routes: [Build a WASM plugin](./build-wasm-module.md)
