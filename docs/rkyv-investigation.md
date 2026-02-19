# Investigation: Replacing serde with rkyv for Serialization

## Summary

**Recommendation: Do not replace serde with rkyv.** The trade-offs are strongly unfavorable for Chatty's use case. rkyv excels at high-throughput, read-heavy workloads with large data structures (game state, trading systems, analytics pipelines). Chatty's serialization is low-frequency, small-payload, human-readable config/persistence — exactly where serde+JSON is the right tool.

---

## Current serde Usage in Chatty

### Serialization Sites (all JSON, all disk persistence)

| What | File | Frequency | Payload Size |
|------|------|-----------|--------------|
| General settings | `general_settings.json` | On settings change | ~100 bytes |
| Provider configs | `providers.json` | On provider CRUD | ~500 bytes |
| Model configs | `models.json` | On model CRUD | ~1-5 KB |
| Execution settings | `execution_settings.json` | On settings change | ~200 bytes |
| MCP server configs | `mcp_servers.json` | On server CRUD | ~500 bytes |
| Conversations | `conversations/{id}.json` | On each message send | ~10-500 KB |

### Serde-Derived Types (~40+ structs/enums across 17 files)

Key types: `ModelConfig`, `ProviderConfig`, `ConversationData`, `UserMessage`, `AssistantMessage`, `SystemTrace`, `TraceItem`, `ThinkingBlock`, `ToolCallBlock`, `ApprovalBlock`, `TokenUsage`, tool argument structs, Ollama API response structs, GitHub release structs.

### Serde Feature Usage

The codebase uses these serde features extensively:
- `#[serde(default)]` and `#[serde(default = "fn")]` — backward-compatible schema evolution
- `#[serde(skip_serializing_if = "Option::is_none")]` — compact JSON output
- `#[serde(skip_serializing_if = "HashMap::is_empty")]` — clean empty-map handling
- `#[serde(rename_all = "snake_case")]` — Rust enum → JSON key mapping
- `#[serde(rename = "...")]` — individual field/variant renaming
- `serde_json::Value` — dynamic/untyped JSON (system traces)
- `serde_json::json!()` — constructing API request bodies (Ollama)
- `serde_json::to_string_pretty()` — human-readable output

---

## Why rkyv Is Not a Good Fit

### 1. External Dependency Barrier (Blocking)

The `rig-core` crate's `Message` type is serialized as part of conversation persistence (`ConversationData.message_history`). rig-core uses serde internally and does **not** support rkyv. There is no `rkyv` feature flag on rig-core, and the types don't derive `rkyv::Archive`.

Options to work around this:
- **Wrapper types with manual conversion**: Creates a parallel type hierarchy that must be kept in sync with upstream changes. Significant maintenance burden.
- **`rkyv-with` crate for remote derives**: Still requires mapping every rig-core type, and rig-core's types are complex (nested enums with `OneOrMany<T>`, etc.).
- **Serialize rig-core types to JSON first, then rkyv the JSON bytes**: Defeats the purpose entirely — you'd still pay the JSON deserialization cost.

This alone makes a full replacement impractical.

### 2. Human Readability Lost

All config files are stored as pretty-printed JSON. Users can (and do) inspect and hand-edit:
- `~/.config/chatty/providers.json` (API keys, base URLs)
- `~/.config/chatty/models.json` (model configurations)
- `~/.config/chatty/general_settings.json` (preferences)

rkyv produces an opaque binary format. Losing human readability means:
- No manual debugging of corrupted configs
- No hand-editing of API keys or settings
- Need to build tooling just to inspect persisted state

### 3. Schema Evolution Is Painful

Chatty actively uses `#[serde(default)]` for backward compatibility when new fields are added. The `ConversationData` struct has multiple `#[serde(default = "fn")]` attributes specifically for loading older conversation files that predate certain features (token usage, attachment paths).

rkyv has **limited schema evolution support**. Changing a struct's fields can make previously-serialized data unreadable. The rkyv documentation itself states: "Enabling non-default format control features should be considered a breaking change." For a desktop app where users accumulate conversation history over months, this is a serious problem.

### 4. No Performance Problem to Solve

The serialization hotpath is saving a conversation after each message. A typical conversation JSON is 10-500 KB. `serde_json` serializes this in **microseconds** — far below perceptible latency. The actual bottleneck is disk I/O (the `tokio::fs::write` call), not serialization.

Benchmark context from rkyv's own benchmarks:
- serde_json deserialize: ~5ms for a complex benchmark dataset
- rkyv access: ~0.002ms for the same dataset

The ~5ms difference is irrelevant when:
- Serialization happens asynchronously (non-blocking UI)
- Disk write latency is 1-10ms
- The operation occurs at most once per user message (seconds apart)

### 5. Increased Complexity

Adding rkyv would require:
- All 40+ data types need `#[derive(Archive, rkyv::Serialize, rkyv::Deserialize)]`
- Wrapper types or conversion layers for rig-core `Message`
- Separate handling for Ollama API responses (which must remain JSON for HTTP)
- Separate handling for tool argument structs (JSON for rig-core tool interface)
- A migration path for existing user data (JSON → rkyv binary)
- Both serde AND rkyv as dependencies (can't remove serde due to rig-core, reqwest, etc.)

### 6. serde Cannot Be Fully Removed

Even if rkyv were adopted for persistence, serde would remain required for:
- `rig-core` integration (Message types, tool definitions)
- `reqwest` JSON request/response bodies (Ollama API, GitHub API)
- `serde_json::Value` for dynamic trace data
- `serde_json::json!()` macro for constructing API payloads
- Tool argument deserialization (rig-core tool interface is serde-based)

You'd be adding a second serialization framework, not replacing one.

---

## Where rkyv Would Make Sense (Hypothetically)

If Chatty's architecture evolved to include any of these, rkyv could be reconsidered:

| Scenario | Why rkyv helps |
|----------|---------------|
| Large vector store for local RAG | Zero-copy access to embeddings without loading entire index |
| Conversation search index | Memory-mapped index for fast full-text search |
| Plugin system with shared memory | Zero-copy IPC between host and plugin processes |
| Caching compiled Typst output | Binary cache of pre-rendered math SVG data |

None of these exist today, and for the current architecture, the overhead of introducing rkyv is not justified.

---

## Alternative Improvements (If Serialization Performance Matters Later)

If serialization performance ever becomes a concern, these approaches are lower-risk:

1. **`simd-json`** — Drop-in replacement for `serde_json` that uses SIMD instructions. Same JSON format, same serde derives, ~2-4x faster parsing. Zero migration cost.

2. **`serde_json::to_writer` instead of `to_string`** — Write directly to a `BufWriter<File>` instead of allocating an intermediate String. Reduces memory allocation.

3. **Incremental/streaming saves** — Only serialize the delta (new messages) instead of the full conversation on each save.

4. **`rmp-serde` (MessagePack)** — Binary format that works with existing serde derives. ~3-5x faster than JSON, compact, but still schema-flexible. Could be used alongside JSON (binary for conversations, JSON for configs).

---

## Conclusion

rkyv is an impressive framework, but it solves a problem Chatty doesn't have. The application's serialization is:
- **Low frequency** (user-driven events, not high-throughput)
- **Small payloads** (KB, not MB/GB)
- **Human-readable by design** (config files users may hand-edit)
- **Schema-evolving** (new features add new fields regularly)
- **Tightly coupled to serde-based dependencies** (rig-core, reqwest)

The cost of adopting rkyv (dual serialization systems, wrapper types, lost readability, migration complexity) far outweighs the negligible performance gain for this use case.
