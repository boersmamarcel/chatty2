---
name: add-provider
description: Step-by-step guide for adding a new LLM provider to Chatty. Use when implementing support for a new AI provider (e.g., Cohere, Perplexity, DeepSeek).
disable-model-invocation: true
allowed-tools: Read, Write, Edit, Grep, Glob, Bash
argument-hint: [provider-name]
---

# Add a New LLM Provider to Chatty

Add support for a new LLM provider named `$ARGUMENTS`. Follow these steps carefully, using the existing providers as reference implementations.

## Phase 1: Understand Existing Patterns

Read these files to understand the current provider architecture:

1. `src/settings/models/providers_store.rs` - `ProviderType` enum and `default_capabilities()`
2. `src/chatty/factories/agent_factory.rs` - Agent creation per provider
3. `src/settings/views/models_settings_view.rs` - Provider selection UI
4. `src/settings/controllers/models_controller.rs` - Model CRUD operations

## Phase 2: Add the Provider Type

1. **Add variant to `ProviderType` enum** in `src/settings/models/providers_store.rs`:
   - Add the new variant (e.g., `DeepSeek`)
   - Update `display_name()`, `from_display_name()`, and `all_variants()`
   - Update `default_capabilities()` with the provider's multimodal support
   - Update `default_base_url()` if the provider has a standard API URL

2. **Update serialization** - ensure the new variant serializes/deserializes correctly with serde

## Phase 3: Implement Agent Factory

1. **Add agent creation** in `src/chatty/factories/agent_factory.rs`:
   - Add a match arm for the new provider in `create_agent()`
   - Use the appropriate rig-core client (check if rig-core supports this provider natively, or if it's OpenAI-compatible)
   - Handle API key, base URL, and model-specific configuration
   - Apply temperature settings based on `supports_temperature`

## Phase 4: Update UI

1. **Settings view** in `src/settings/views/models_settings_view.rs`:
   - The provider should automatically appear in the dropdown since `ProviderType::all_variants()` is used
   - Verify the provider name displays correctly

## Phase 5: Test

1. Run `cargo build` to ensure compilation
2. Run `cargo clippy -- -D warnings` to check for lint issues
3. Run `cargo test` to verify existing tests still pass

## Notes

- If the provider uses an OpenAI-compatible API, you can reuse the OpenAI client with a custom base URL
- Check if the provider requires any special authentication headers
- Update CLAUDE.md Model Capability Architecture section if adding non-obvious capabilities
- Add the provider's default models if known (common model IDs)
