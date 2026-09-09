# Add an LLM provider

**When to read this:** Integrate a new LLM API into Chatty — or find out that you do not need to, because the endpoint already speaks an API Chatty supports.

## Goal

A new `ProviderType` that can be configured in Settings, builds a rig agent with the full native and MCP tool set, streams, reports token usage correctly, and declares sensible default capabilities for its models.

## Prerequisites

- [Build and run](../start/build-and-run.md) done; `make test-fast` green.
- Know the three current providers and how they differ: the [provider matrix](../reference/provider-matrix.md).
- Files to read first: [`providers_store.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-core/src/settings/models/providers_store.rs) (`ProviderType`, `ProviderConfig`, `configured_providers`), [`provider_builder.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-core/src/factories/agent_factory/provider_builder.rs) (one match arm per provider), [`llm_service.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-core/src/services/llm_service.rs) (streaming and usage normalisation).

> [!TIP]
> **You may not need a new variant.** Any OpenAI-compatible chat-completions endpoint (vLLM, llama.cpp, LM Studio, Groq, Together, …) already works as `ProviderType::OpenRouter` with a custom `base_url`; that is exactly what `chatty-tui --openai-compat-url` injects. Add a variant only when the wire format, auth or usage accounting genuinely differ.

## Steps

### 1. Add the `ProviderType` variant

In `crates/chatty-core/src/settings/models/providers_store.rs`:

1. Add the variant. The enum is `#[serde(rename_all = "snake_case")]`, so the variant name *is* the persisted JSON string (`AzureOpenAI` uses an explicit `rename = "azure_openai"`). Do not rename an existing variant without a `#[serde(alias)]`; users' saved settings would stop loading.
2. Add its arm to `display_name()`.
3. Add its arm to `default_capabilities()` — `(supports_images, supports_pdf)` for models created under it: `(true, true)`, `(true, false)` or `(false, false)`.
4. Decide what counts as configured in `configured_providers()`. The default arm requires a non-empty `api_key`; Ollama is always included; Azure needs `base_url` plus a key or Entra ID.

### 2. Let the compiler find the other matches

```bash
cargo check --all-features
```

Every non-exhaustive `match` fails to compile. Today those are `build_provider_agent` in `provider_builder.rs` (step 3), the provider string in [`exporters/atif_exporter/steps.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-core/src/exporters/atif_exporter/steps.rs), and on the desktop the display-name-to-type mapping and provider list in [`settings/views/models_page/mod.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-gpui/src/settings/views/models_page/mod.rs) and the per-provider model catalog in [`add_model_sheet.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-gpui/src/settings/views/models_page/add_model_sheet.rs).

### 3. Build the agent

Add a match arm in `build_provider_agent` (`crates/chatty-core/src/factories/agent_factory/provider_builder.rs`). All tool construction happens before this function; the arm only creates the client, configures the builder and attaches tools. The Ollama arm is the smallest template:

```rust
ProviderType::MyProvider => {
    let key = api_key.ok_or_else(|| anyhow!("API key not configured for MyProvider"))?;
    let client = rig_core::providers::my_provider::Client::builder()
        .api_key(&key)
        .build()?;

    let mut builder = client
        .agent(&model_config.model_identifier)
        .preamble(preamble);
    if model_config.supports_temperature {
        builder = builder.temperature(model_config.temperature as f64);
    }
    if let Some(max_tokens) = model_config.max_tokens {
        builder = builder.max_tokens(max_tokens as u64);
    }

    let builder = native_tools.apply_to_builder(builder);
    let agent = build_with_mcp_tools!(builder, mcp_tools, native_tool_names);
    Ok(AgentClient { agent, task_controller, provider: ProviderType::MyProvider })
}
```

Points that differ per provider:

- **Client.** Prefer a native `rig_core::providers::*` client when rig has one; otherwise reuse the OpenAI-compatible path with `base_url`. Check the rig pins in `Cargo.toml` before relying on a new provider module.
- **Auth.** `ProviderConfig` carries `api_key`, `base_url` and a free-form `extra_config` map (Azure keeps its `auth_method` there). If a credential must be refreshed per request, wrap the HTTP client the way [`azure_auth_http.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-core/src/factories/agent_factory/azure_auth_http.rs) does rather than awaiting a token inside the stream handler; `on_chunk` is synchronous.
- **Temperature.** Only when `model_config.supports_temperature`; reasoning models reject it.
- **OpenAI-family schemas.** Call `sanitize_mcp_tools_for_openai(mcp_tools)` before attaching MCP tools, as the OpenRouter arm does.
- **Prompt caching.** The OpenRouter arm builds from `completion_model(..).with_prompt_caching()` and a `PromptCachingHttpClient`; copy that only if the upstream honours `cache_control` breakpoints.

### 4. Check token-usage semantics

`stream_prompt` in `llm_service.rs` normalises every provider request's usage with `normalize_usage(UsageSemantics, …)`. It is currently hard-coded to `UsageSemantics::InputIncludesCache` because OpenRouter, Azure and Ollama all report OpenAI-style numbers (cached tokens inside `prompt_tokens`). A provider that reports cached tokens separately (Anthropic-native) needs `InputExcludesCache`, selected from `agent.provider`; otherwise `input + cache_read + cache_write` no longer equals the prompt and the cost and hit-rate figures are wrong. The `LLM completion call usage` log line is the check ([Debug](./debug.md)).

### 5. Settings UI (desktop)

- [`settings/views/models_page/provider_keys_sheet.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-gpui/src/settings/views/models_page/provider_keys_sheet.rs) — one section per provider: key and URL inputs, status line, test-connection label. Add yours.
- [`settings/controllers/providers_controller.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-gpui/src/settings/controllers/providers_controller.rs) — creates or updates the `ProviderConfig` and persists it through the existing JSON repository; no new repository is needed.
- `add_model_sheet.rs` — the model catalog shown when adding a model (OpenRouter has one; Ollama and Azure show `Catalog::Unavailable`).

### 6. Optional: model discovery

If the API lists models, add `crates/chatty-gpui/src/settings/providers/<name>/sync_service.rs` following [`ollama/sync_service.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-gpui/src/settings/providers/ollama/sync_service.rs) or `openrouter/sync_service.rs`. Keep the HTTP discovery itself UI-agnostic in `chatty-core` (`settings/providers/ollama/discovery.rs` is the precedent) so the TUI can reuse it for a direct-connect flag (`discover_*` + `inject_discovered` in [`chatty-tui/src/main.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-tui/src/main.rs)). Without discovery, users type model ids by hand.

### 7. Update the references

Add the provider to the provider-matrix block in `scripts/gen-docs-reference.sh` and run `make docs-gen`; describe it for users in [Providers & models](../../user/providers-and-models.md).

### 8. Test

```bash
cargo test --all-features -- --test-threads=1
cargo clippy --all-features -- -D warnings
```

Then manually: configure the provider, add a model, send a message and watch it stream; trigger a tool call; attach an image or PDF according to the capabilities you declared.

## Verify

- `ProviderType` round-trips through serde with the string you intended (`settings/models/mod.rs` has the round-trip tests for the existing variants; add one).
- A conversation streams text and completes a tool call.
- The `LLM completion call usage` line shows `input + cache_read + cache_write` equal to the prompt size the provider reports.
- Attachments are offered only for the capabilities you declared.

## Checklist

- [ ] `ProviderType` variant, `display_name`, `default_capabilities`, `configured_providers` rule
- [ ] Every `match` updated (`cargo check --all-features` clean)
- [ ] `build_provider_agent` arm: client, temperature gate, tools, MCP
- [ ] Usage semantics correct for the provider's accounting
- [ ] Provider section in `provider_keys_sheet.rs`; controller persists it
- [ ] Provider matrix regenerated; user page updated
- [ ] `cargo test --all-features` and clippy pass; manual chat, tool call and attachment checks done

## Common mistakes

| Mistake | Do this instead |
|---------|-----------------|
| Adding a variant for an OpenAI-compatible endpoint | `OpenRouter` + `base_url` |
| Renaming a persisted variant | Keep a `#[serde(alias)]` for the old string |
| Setting temperature unconditionally | Gate on `supports_temperature` |
| Skipping `sanitize_mcp_tools_for_openai` on an OpenAI-family API | Sanitise; the request fails on schema `format` fields otherwise |
| Leaving `InputIncludesCache` for an Anthropic-style API | Choose semantics by `agent.provider` |
| Refreshing a token inside the stream | Per-request HTTP wrapper, like Azure |
| Checking capabilities from `ProviderType` at runtime | Read `ModelConfig`; provider defaults are for creation only |
