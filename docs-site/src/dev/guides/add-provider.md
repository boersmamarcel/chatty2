# Add a new LLM provider

**When to read this:** Integrate a new LLM API into Chatty.

## Steps

1. Add a variant to `ProviderType` in `crates/chatty-core/src/settings/models/providers_store.rs`
2. Implement `default_capabilities()` for the new provider
3. Add an agent builder module under `crates/chatty-core/src/factories/agent_factory/`
4. Wire the builder in `agent_factory/mod.rs`
5. Add settings UI in `crates/chatty-gpui/src/settings/views/` (provider form section)
6. Add persistence via existing JSON repositories

## Patterns

- Temperature: only set when `ModelConfig.supports_temperature`
- Auth: Azure uses `auth/azure_auth.rs`; API keys live in `ProviderModel`
- Capabilities: runtime checks use **ModelConfig**, not provider defaults alone

See also `.claude/skills/add-provider/SKILL.md` in the repo.
