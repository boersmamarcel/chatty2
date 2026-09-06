# Add a new LLM tool

**When to read this:** Expose a new capability the LLM can invoke.

## Steps

1. Create `crates/chatty-core/src/tools/my_tool.rs` implementing Rig's `Tool` trait
2. Export from `tools/mod.rs`
3. Register in `factories/agent_factory/tool_collector.rs`
4. Add name to `tool_registry.rs` if gated by `ToolAvailability`
5. Add tests (including Gemini schema compat in `tools/mod.rs` test module)

## Approval

Tools with side effects (shell, writes) must go through `ExecutionApprovalStore` /
`WriteApprovalStore`. Read-only tools can skip approval.

## Reference

Full tool list: [tools catalog](../reference/tools-catalog.md)
