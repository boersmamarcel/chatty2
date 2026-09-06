# Your first change: add a tool

**When to read this:** You have [built and run](./build-and-run.md) Chatty and want to make a real, end-to-end change: expose a new capability the model can call.

## Goal

A `word_count` tool the agent can invoke, wired the way every built-in tool is wired, with its error path routed through `map_tool_error`, its schema guarded by the Gemini-compatibility test, its name in the registry, and its row in the tools catalog. Same shape, bigger tool, next time.

## Prerequisites

- `make test-fast` passes on your checkout.
- Read the tool error section of [Contributing patterns](../contributing-patterns.md) once; it explains *why* step 2 is a hard rule.
- Two files open for reference: [`remember_tool.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-core/src/tools/remember_tool.rs) (small, stateful) and [`execute_code_tool.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-core/src/tools/execute_code_tool.rs) (typed output). The skeleton below is theirs with the business logic removed.

## Steps

### 1. Write the tool

Create `crates/chatty-core/src/tools/word_count_tool.rs`:

```rust
use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};

use crate::tools::ToolError;

#[derive(Debug, Deserialize, Serialize)]
pub struct WordCountArgs {
    /// The text to count
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct WordCountOutput {
    pub words: usize,
}

/// Count the words in a piece of text.
#[derive(Clone, Default)]
pub struct WordCountTool;

impl Tool for WordCountTool {
    const NAME: &'static str = "word_count";
    type Error = ToolError;
    type Args = WordCountArgs;
    type Output = WordCountOutput;

    fn description(&self) -> String {
        "Count the whitespace-separated words in a piece of text.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The text to count"
                }
            },
            "required": ["text"]
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed".
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        if args.text.trim().is_empty() {
            return Err(ToolError::OperationFailed("text is empty".into()));
        }
        Ok(WordCountOutput {
            words: args.text.split_whitespace().count(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn counts_words() {
        let mut ctx = ToolContext::new();
        let out = WordCountTool
            .call(&mut ctx, WordCountArgs { text: "one two  three".into() })
            .await
            .unwrap();
        assert_eq!(out.words, 3);
    }

    #[test]
    fn empty_text_is_an_error_the_model_can_read() {
        let err = WordCountTool.map_error(ToolError::OperationFailed("text is empty".into()));
        let feedback = err.model_feedback().unwrap_or_default();
        assert!(feedback.starts_with("Error: word_count: "), "{feedback}");
    }
}
```

`ToolError` is the shared one-variant error in `tools/mod.rs`; use it unless your tool has genuinely distinct failure categories. Every JSON schema object needs a `type` (step 5 checks this).

### 2. Route the error path through `map_tool_error`

The `map_error` override above is not optional. rig's default `Tool::map_error` redacts any source error to a per-kind message — for the common `Other` kind that message is the literal string `"the tool failed"`, and that is all the model and the transcript ever see. `map_tool_error(tool_name, error)` in [`tools/mod.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-core/src/tools/mod.rs) keeps the message and writes it as `Error: {tool_name}: {message}`.

That prefix is also load-bearing: the streamed tool result carries no error flag, so `llm_service::tool_result_looks_like_error` recognises a failure by the `Error:` prefix. A tool that bypasses `map_tool_error` is filed as a success, with its error text in `output`. Only route errors you authored through it; raw provider or third-party error text is what rig's redaction exists for.

### 3. Export it

In `crates/chatty-core/src/tools/mod.rs`, next to the other modules:

```rust
pub mod word_count_tool;
// …
pub use word_count_tool::WordCountTool;
```

### 4. Register it

Tool registration is spread over three files in `crates/chatty-core/src/factories/agent_factory/`; the compiler walks you through each.

1. **[`tool_collector.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-core/src/factories/agent_factory/tool_collector.rs)** — add a field to `NativeTools`, a `.tool(t)` call in `apply_to_builder`, and the field to the `native_tools!` macro (both its parameter list and the struct literal it expands to). rig registers typed tools with repeated `.tool(T)` calls, which is why the struct exists:

   ```rust
   pub word_count_tool: Option<WordCountTool>,
   // in apply_to_builder:
   if let Some(t) = self.word_count_tool {
       b = b.tool(t);
   }
   ```

2. **[`mod.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-core/src/factories/agent_factory/mod.rs)** (`from_model_config_with_tools`) — construct the tool where the others are built, gated on whatever setting should enable it (`Some(WordCountTool)` for an always-on tool), and pass it in the single `native_tools!(…)` call.

3. **[`tool_registry.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-core/src/factories/agent_factory/tool_registry.rs)** — add a `word_count: bool` flag to `ToolAvailability` and insert `"word_count"` in `active_native_tool_names` when it is set (or add the name to the always-on set at the top of that function). Then set the flag in both `ToolAvailability { … }` literals in `mod.rs`. These names are what `list_tools` reports, what the preamble's tool summary lists, and what `filter_mcp_tool_info` uses to drop MCP tools that would collide with a native name — a tool that is registered but not named here is invisible to the model's own inventory. The registry tests spell out every flag (`all_flags_enabled_produces_superset`), so the new field breaks them until you add it; if you chose the always-on set, `always_includes_baseline_tools` counts them.

### 5. Add the Gemini schema guard

Gemini rejects any schema node with an empty `type`. `gemini_compat_tests` in `tools/mod.rs` converts each tool's definition to a Gemini `Tool` and walks the result. Add one:

```rust
#[tokio::test]
async fn word_count_tool_gemini_compat() {
    use crate::tools::word_count_tool::WordCountTool;
    check_gemini_compat(rig_agent::tool::tool_definition(&WordCountTool));
}
```

### 6. Add the catalog row

`make docs-check-reference` diffs every `const NAME` under `crates/chatty-core/src/tools/` against the `tools = [` table in `scripts/gen-docs-reference.sh`, so CI fails without a row. Add one (`("word_count", "text", "word_count_tool.rs", "Always available")` follows the existing shape), then `make docs-gen` regenerates the [tools catalog](../reference/tools-catalog.md).

### 7. Run it

```bash
cargo test -p chatty-core --lib word_count      # your tests + the gemini guard
make test-fast
cargo run -p chatty-tui -- --ollama --headless -m "Use the word_count tool on: the quick brown fox"
make ci
```

## Verify

- Both unit tests and `word_count_tool_gemini_compat` pass.
- `/tools` in the TUI (or `list_tools` in a conversation) shows `word_count`.
- Asking for a count produces a tool call in the transcript; an empty string produces `Error: word_count: text is empty` in the tool row, not "the tool failed".
- `make ci` and `make docs-check-reference` pass.

## Checklist

- [ ] `impl Tool` with `const NAME`, `description`, `parameters`, `call`
- [ ] `map_error` delegates to `map_tool_error(Self::NAME, error)`
- [ ] Exported from `tools/mod.rs`
- [ ] `NativeTools` field, `apply_to_builder` call, `native_tools!` macro entry
- [ ] Constructed in `from_model_config_with_tools`, flag set in both `ToolAvailability` literals
- [ ] Name in `active_native_tool_names`
- [ ] `gemini_compat` test added
- [ ] Row in `scripts/gen-docs-reference.sh`, `make docs-gen` run
- [ ] Side effects go through an approval store (below)

## Approval

Tools with side effects — running commands, writing files — must ask before acting. Shell tools go through `ExecutionApprovalStore` ([`models/execution_approval_store.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-core/src/models/execution_approval_store.rs)); file writes through `WriteApprovalStore` ([`models/write_approval_store.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-core/src/models/write_approval_store.rs)). Copy the pattern from `shell_tool.rs` or `filesystem_write_tool.rs`. Read-only tools like `word_count` skip approval.

## Common mistakes

| Mistake | Consequence | Fix |
|---------|-------------|-----|
| No `map_error` override | Model and transcript see "the tool failed" | Delegate to `map_tool_error` |
| Rewording the `Error:` prefix | Failure filed as success, error prose in `output` | Keep `map_tool_error`'s format |
| Registered in `tool_collector.rs` but not named in `tool_registry.rs` | Missing from `list_tools`; an MCP tool with the same name is not filtered | Add the name |
| Schema object without `type` | Gemini rejects the request | Run the `gemini_compat` test |
| Forgot `scripts/gen-docs-reference.sh` | `docs-check-reference` fails in CI | Add the row, `make docs-gen` |
| Routing a provider's raw error through `map_tool_error` | Leaks text the model should not act on | Only your own error strings |
