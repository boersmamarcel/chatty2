---
description: Guide for adding a new tool that the LLM can invoke during conversations. Use when implementing new tool capabilities (e.g., a search tool, calculator, database tool).
user-invocable: true
---

# Add New LLM Tool

This skill walks through creating a new tool that the LLM can call during chat conversations.

## Step-by-step Checklist

### 1. Create the Tool File

**Directory**: `src/chatty/tools/`

Create a new file (e.g., `my_tool.rs`) following this structure:

```rust
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};

/// Arguments the LLM provides when calling this tool
#[derive(Deserialize, Serialize)]
pub struct MyToolArgs {
    pub param1: String,
    #[serde(default)]
    pub optional_param: Option<i32>,
}

/// Output returned to the LLM
#[derive(Debug, Serialize)]
pub struct MyToolOutput {
    pub result: String,
}

/// Error type
#[derive(Debug, thiserror::Error)]
pub enum MyToolError {
    #[error("Tool error: {0}")]
    GeneralError(String),
}

/// The tool implementation
#[derive(Clone)]
pub struct MyTool {
    // Any state the tool needs (workspace path, config, etc.)
}

impl MyTool {
    pub fn new() -> Self {
        Self {}
    }
}

impl Tool for MyTool {
    const NAME: &'static str = "my_tool";

    type Error = MyToolError;
    type Args = MyToolArgs;
    type Output = MyToolOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Clear description of what this tool does for the LLM".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "param1": {
                        "type": "string",
                        "description": "What this parameter is for"
                    },
                    "optional_param": {
                        "type": "integer",
                        "description": "Optional parameter description"
                    }
                },
                "required": ["param1"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Tool implementation here
        Ok(MyToolOutput {
            result: format!("Processed: {}", args.param1),
        })
    }
}
```

### 2. Register in Module

**File**: `src/chatty/tools/mod.rs`

Add the module declaration and re-export:

```rust
pub mod my_tool;
pub use my_tool::MyTool;
```

### 3. Add to Agent Factory

**File**: `src/chatty/factories/agent_factory.rs`

Add the tool to the agent builder. Follow the existing patterns:

- If the tool should always be available, add it alongside the filesystem tools
- If it should be gated on a setting, add a conditional check
- Import the tool at the top of the file

The tool is attached via the rig-core builder's `.tool()` method.

### 4. Handle Tool Calls in Stream (if needed)

**File**: `src/chatty/services/llm_service.rs`

The `process_agent_stream!` macro already handles generic tool calls via `StreamItem::ToolCall`. If the tool needs special stream handling (e.g., progress updates), modify the stream processing.

**File**: `src/chatty/models/stream_manager.rs`

`StreamManagerEvent::ToolCallStarted` and `ToolCallCompleted` events are already emitted for all tools. If the tool needs custom UI (beyond the generic tool trace display), add a new event variant.

### 5. Add Approval Flow (if the tool has side effects)

**Files**:
- `src/chatty/models/execution_approval_store.rs` — approval state management
- `src/chatty/views/approval_prompt_bar.rs` — approval UI
- `src/settings/models/execution_settings.rs` — approval mode config

If the tool modifies state (writes files, executes commands, makes network requests), it should respect the execution approval settings.

### 6. Test

- Run `cargo test` and `cargo clippy -- -D warnings`
- Test the tool in a conversation by prompting the LLM to use it
- Verify tool calls appear in the trace UI
- Test error cases

## Key Architecture Rules

- Tools are stateless per-call but can hold shared state (like `reqwest::Client`)
- Tool output is serialized to JSON and sent back to the LLM
- Use `thiserror` for error types
- Security: validate all inputs, especially file paths (use `PathValidator`) and URLs (check for SSRF)
- Never expose sensitive data (API keys, tokens) in tool output
