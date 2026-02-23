---
description: Guide for adding a new LLM provider to Chatty. Use when implementing support for a new AI provider (e.g., Cohere, DeepSeek, xAI).
user-invocable: true
---

# Add New LLM Provider

This skill walks through all the files that must be modified to add a new LLM provider to Chatty.

## Step-by-step Checklist

### 1. Add Provider Type Enum Variant

**File**: `src/settings/models/providers_store.rs`

Add a new variant to the `ProviderType` enum:

```rust
pub enum ProviderType {
    OpenAI,
    Anthropic,
    Gemini,
    Mistral,
    Ollama,
    AzureOpenAI,
    NewProvider,  // <-- Add here
}
```

Update these methods on `ProviderType`:
- `display_name()` — human-readable name for the UI
- `default_capabilities()` — return `(supports_images, supports_pdf)` defaults

### 2. Add Provider Config Handling

**File**: `src/settings/models/providers_store.rs`

The `ProviderConfig` struct is generic and works for all providers via `api_key`, `base_url`, and `extra_config`. No changes needed unless the provider requires special fields.

### 3. Build the LLM Agent

**File**: `src/chatty/factories/agent_factory.rs`

This is the core integration point. Add a new match arm in the agent-building logic to:
- Create the provider's rig-core client (check if rig-core supports it, or use the OpenAI-compatible client)
- Configure API key and base URL from `ProviderConfig`
- Set temperature (if `model_config.supports_temperature`)
- Set preamble, max_tokens, and other parameters from `ModelConfig`
- Attach tools (filesystem, shell, fetch, MCP, etc.)

Pattern: Follow the existing OpenAI or Anthropic arms as a template.

### 4. Add Settings UI

**File**: `src/settings/views/providers_view.rs`

Add the new provider to the provider selection dropdown and any provider-specific configuration fields (e.g., custom base URL, deployment name for Azure-style providers).

### 5. Update Models Controller Defaults

**File**: `src/settings/controllers/models_controller.rs`

The `create_model()` function applies `ProviderType::default_capabilities()` automatically. No changes needed unless the provider has special model creation logic.

### 6. Handle Stream Processing

**File**: `src/chatty/services/llm_service.rs`

If the provider uses rig-core's standard agent interface, the existing `process_agent_stream!` macro handles streaming automatically. If the provider has a custom streaming format, add a new match arm.

### 7. Test

- Add a provider config via Settings UI
- Create a model pointing to the new provider
- Send a test message and verify streaming works
- Test tool calls if the provider supports them
- Run `cargo test` and `cargo clippy -- -D warnings`

## Key Architecture Rules

- Provider capabilities are set in TWO layers: `ProviderType::default_capabilities()` for defaults, `ModelConfig` for per-model overrides
- Never expose `api_key` to the LLM — it is only used for provider API authentication
- The Tokio runtime is already entered at startup; async operations work via `cx.spawn()`
- Use the optimistic update pattern: update global state immediately, persist asynchronously
