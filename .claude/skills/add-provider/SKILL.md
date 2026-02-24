---
name: add-provider
description: Guides the implementation of a new LLM provider for Chatty. Covers all required changes from ProviderType enum to agent factory integration. Use when adding support for a new AI provider like Cohere, Groq, or DeepSeek.
argument-hint: "[provider-name]"
disable-model-invocation: true
allowed-tools: Bash, Read, Grep, Glob, Edit, Write
---

# Add New LLM Provider

Step-by-step guide for adding a new LLM provider to Chatty. The provider name is `$ARGUMENTS`.

## Prerequisites

Before starting, read these key files to understand current implementations:

- `src/settings/models/providers_store.rs` — `ProviderType` enum and capabilities
- `src/chatty/factories/agent_factory.rs` — Agent creation for each provider
- `src/chatty/services/llm_service.rs` — Stream processing
- `src/settings/views/models_settings_view.rs` — Settings UI for model configuration

## Implementation Steps

### Step 1: Add ProviderType Variant

In `src/settings/models/providers_store.rs`:

1. Add the new variant to the `ProviderType` enum
2. Update `ProviderType::display_name()` to return a human-readable name
3. Update `ProviderType::default_capabilities()` with the provider's defaults:
   - `(true, true)` — Supports images and PDFs
   - `(true, false)` — Supports images only
   - `(false, false)` — No multimodal support
4. Update `ProviderType::default_base_url()` if the provider has a standard API base URL
5. Update any `match` expressions on `ProviderType` that need the new variant

### Step 2: Add to Agent Factory

In `src/chatty/factories/agent_factory.rs`:

1. Add a new match arm in `create_agent()` for the provider
2. Use the appropriate rig-core client (check if rig-core supports the provider natively, or if it's OpenAI-compatible)
3. Handle authentication (API key from `ProviderConfig`)
4. Handle temperature setting (check `supports_temperature` from `ModelConfig`)
5. Handle tool definitions if the provider supports function calling

### Step 3: Update Stream Processing

In `src/chatty/services/llm_service.rs`:

1. If the provider uses a rig-core native client, the existing `process_agent_stream!` macro should work
2. If the provider needs custom stream handling, add a new match arm in `stream_prompt()`
3. Test that `StreamChunk` variants map correctly to the provider's response format

### Step 4: Add Provider to Settings UI

In `src/settings/views/models_settings_view.rs`:

1. The provider should automatically appear in the provider dropdown since it's part of `ProviderType`
2. Verify the settings form fields are appropriate (API key, base URL, etc.)

### Step 5: Update Sync Service (if applicable)

In `src/chatty/services/sync_service.rs`:

1. If the provider has an API to list available models (like Ollama's `/api/tags`), add auto-discovery support
2. Otherwise, users will manually add model IDs in settings

### Step 6: Test

1. Add the provider configuration in settings
2. Create a model using the new provider
3. Send a test message and verify streaming works
4. Test tool calling if supported
5. Test image/PDF attachments based on declared capabilities
6. Run `cargo test --all-features` and `cargo clippy -- -D warnings`

## Common Patterns

### OpenAI-Compatible Providers

Many providers (Groq, Together, Perplexity, etc.) use OpenAI-compatible APIs. For these:

```rust
// In agent_factory.rs, reuse the OpenAI client with a custom base URL
let client = openai::Client::from_url(&config.base_url, &config.api_key);
```

### Providers with Native rig-core Support

Before defaulting to the OpenAI-compatible path, check whether rig-core has a native client for the provider. Native clients offer better type safety and may support provider-specific features. Check the existing match arms in `agent_factory.rs` for examples (Anthropic, Gemini have native clients; others use the OpenAI-compatible path).

## Checklist

- [ ] `ProviderType` variant added with all match arms updated
- [ ] `default_capabilities()` returns correct values
- [ ] Agent factory creates clients for the new provider
- [ ] Stream processing works correctly
- [ ] Settings UI shows the provider
- [ ] Manual testing passes (chat, streaming, tools if applicable)
- [ ] `cargo test --all-features` passes
- [ ] `cargo clippy -- -D warnings` passes
