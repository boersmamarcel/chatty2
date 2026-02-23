---
name: add-tool
description: Step-by-step guide for adding a new tool to Chatty's tool system. Use when implementing a new tool that the LLM can invoke during conversations.
disable-model-invocation: true
allowed-tools: Read, Write, Edit, Grep, Glob, Bash
argument-hint: [tool-name]
---

# Add a New Tool to Chatty

Add a new tool named `$ARGUMENTS` to the Chatty tool system. Follow these steps, using existing tools as reference.

## Phase 1: Understand the Tool System

Read these files to understand the current tool architecture:

1. `src/chatty/tools/` - Existing tool implementations (filesystem, bash, MCP management)
2. `src/chatty/factories/agent_factory.rs` - Where tools are registered with agents
3. `src/chatty/services/llm_service.rs` - How tool calls are processed in streams

## Phase 2: Create the Tool Implementation

1. **Create a new file** in `src/chatty/tools/` (e.g., `my_tool.rs`)

2. **Define the tool struct** using rig-core's tool traits:
   - Implement `rig::tool::Tool` trait
   - Define input/output types with `serde::Deserialize` / `serde::Serialize`
   - Provide a clear `definition()` with name, description, and JSON schema for parameters
   - Implement `call()` with the tool's logic

3. **Follow existing patterns**:
   - Use descriptive error messages
   - Return structured output the LLM can parse
   - If the tool accesses the filesystem, respect workspace boundaries from `ExecutionSettingsModel`
   - If the tool has side effects, integrate with the approval system (`ExecutionApprovalStore` / `WriteApprovalStore`)

## Phase 3: Register the Tool

1. **Export the module** in `src/chatty/tools/mod.rs`

2. **Register with agents** in `src/chatty/factories/agent_factory.rs`:
   - Add the tool to the agent builder in `create_agent()`
   - Consider whether it should be available to all providers or conditionally

## Phase 4: Handle Tool Calls in Streaming (if needed)

If the tool requires special handling in the UI (beyond default tool call display):

1. Check `src/chatty/views/chat_view.rs` for tool call rendering
2. Add custom rendering if the tool output needs special formatting

## Phase 5: Verify

1. Run `cargo build` to ensure compilation
2. Run `cargo clippy -- -D warnings`
3. Run `cargo test`
4. Test the tool manually by asking the LLM to use it in a conversation
