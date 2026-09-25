# chatty-core API changes on `harness-dynamic-tools` (for hive's next re-pin)

This branch stack removed the default agent turn cap, added a headless
`--max-duration` budget and a `TurnBudget` hook, added on-demand tool
loading (`--tool-loading`), and touched a few public `chatty-core` items
along the way. Each item below was verified by grepping this worktree
(not guessed), with the one-line fix hive needs on its next rev-pin.

## `STALLED_STREAM_MESSAGE` → `stalled_stream_message(timeout)`

The `const STALLED_STREAM_MESSAGE: &str` in
`crates/chatty-core/src/services/stream_processor.rs` is gone; there is
now a function:

```rust
pub fn stalled_stream_message(timeout: std::time::Duration) -> String
```

(`crates/chatty-core/src/services/stream_processor.rs:274`, re-exported
from `chatty_core::services`.) The stall message now names the timeout
that fired (tool-call stalls and idle-stream stalls use different
durations), which a fixed constant could not do.

**Hive fix**: replace any reference to `chatty_core::services::STALLED_STREAM_MESSAGE`
with a call `chatty_core::services::stalled_stream_message(timeout)`,
passing whatever `Duration` hive already uses for its own stall/timeout
comparison (or the value it was hard-coding to match the old constant's text).

## `TurnInput.turn_budget` (new field)

`crates/chatty-core/src/session/mod.rs:131` — `TurnInput` gained:

```rust
pub turn_budget: Option<TurnBudget>,
```

Doc comment: "This turn's tool-turn budget when it is not the configured
`max_agent_turns`: headless runs its follow-up passes on what is left of
the run's budget." This is additive — existing construction sites need a
value, so any hive code that builds a `TurnInput` by name (not `..Default::default()`
or a builder) will fail to compile until the field is filled in.

**Hive fix**: if hive constructs `TurnInput` with a struct literal, add
`turn_budget: None` (single-pass run, uses the configured `max_agent_turns`
as before) unless hive itself wants to hand the turn a remaining-budget
value from its own run loop.

## `AgentBuildContext.unattended` and `AgentBuildContext::answer_file` (new fields)

`crates/chatty-core/src/factories/agent_factory/build_context.rs:88,94`:

```rust
pub unattended: bool,
pub answer_file: Option<bool>,
```

`unattended` tells the system prompt nobody is watching the run (headless,
pipe, a delegated worker) so the model finishes the work instead of
offering to continue. `answer_file` is `Some(false)` when the run is known
in advance not to want an `answer.txt` from `final_answer` (a coding run),
`None` everywhere else (writes as before). `AgentBuildContext` has no
`Default`; `AgentBuildContext::from_services()` sets both to `false`/`None`
(`build_context.rs:212,214`), so construction via
`..AgentBuildContext::from_services(services)` is unaffected, while a full
struct literal fails to compile until both fields are filled in.

**Hive fix**: no change required if hive builds `AgentBuildContext` on top
of `from_services()` and doesn't need the behavior; a full struct literal
needs `unattended: false, answer_file: None`. If hive runs
agents unattended (no human to answer `ask_user` or accept a continue
offer), set `unattended: true` explicitly to get the same finish-the-work
prompt language chatty-tui's headless/pipe modes get.

## `ExecutionSettingsModel::tool_loading` (new field)

`crates/chatty-core/src/settings/models/execution_settings.rs:149`:

```rust
#[serde(default, skip_serializing_if = "ToolLoading::is_all")]
pub tool_loading: ToolLoading,
```

Defaults to `ToolLoading::All` (every enabled tool's schema sent with
every request — today's behavior) and is skipped on serialization when at
that default, so existing persisted settings files/snapshots are
byte-for-byte unchanged until something opts into `ToolLoading::Dynamic`.

**Hive fix**: no change required to read persisted settings (the field
deserializes to `All` when absent). If hive constructs
`ExecutionSettingsModel` with a struct literal instead of `Default`, add
`tool_loading: ToolLoading::All` (or `Default::default()`).

## `ExecutionSettingsModel::ask_user_enabled` (new field)

`crates/chatty-core/src/settings/models/execution_settings.rs`:

```rust
#[serde(default = "default_true", skip_serializing_if = "is_true")]
pub ask_user_enabled: bool,
```

Gates the `ask_user` tool in `agent_factory` (`chatty-tui --disable
ask-user`). Defaults to `true` and deserializes to `true` when absent, so
persisted settings keep offering the tool. Like `tool_loading` it is skipped
on serialization at its default, so settings files and `SettingsSnapshot`
bytes are unchanged unless a run turns it off. The
gate only applies when the agent gets execution settings at all:
`gated_exec_settings` returns `None` when every execution group is off, and
`None` offers `ask_user` whenever a clarification store exists.

**Hive fix**: a struct-literal `ExecutionSettingsModel` needs
`ask_user_enabled: true` (or `..Default::default()`). A hosted worker that
must never park on a question can set it `false`.

## `AgentClient::supports_images()` (new method)

Additive. `stream_prompt` now replaces image content (new message, history,
tool results) with a text note when the agent's `ModelConfig::supports_images`
was `false` at build time. No hive change needed, but a hosted model whose
config says `supports_images: false` no longer sends images at all.

## `stream_prompt` now takes a `TurnBudget`

`crates/chatty-core/src/services/llm_service.rs:496`:

```rust
pub async fn stream_prompt(
    agent: &AgentClient,
    history: Vec<Message>,
    contents: Vec<UserContent>,
    approval_rx: Option<mpsc::UnboundedReceiver<ApprovalNotification>>,
    resolution_rx: Option<mpsc::UnboundedReceiver<ApprovalResolution>>,
    clarification_rx: Option<mpsc::UnboundedReceiver<ClarificationNotification>>,
    turn_budget: TurnBudget,
) -> Result<ResponseStream>
```

`turn_budget` is a new trailing parameter (was previously derived
internally from `max_agent_turns`). It sets rig's per-call turn cap
(tool turns plus one tool-free wrap-up call) and tells the model how many
tool turns remain.

**Hive fix**: any direct caller of `stream_prompt` needs to pass
`TurnBudget::new(max_agent_turns as usize)`
(`chatty_core::services::turn_budget::TurnBudget`; `max_agent_turns` is a
`u32` on `ExecutionSettingsModel`) at the call site (same value the old
internal default used), or a smaller budget if hive is resuming a run that
already spent part of its turn allowance.
