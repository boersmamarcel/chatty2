# Pre-Built APIs

This document catalogs public APIs that are defined but not yet wired to callers. They are marked with `#[allow(dead_code)]` in the source and exist as scaffolding for planned features. Contributors should wire these APIs rather than reimplementing the functionality.

## Write Approval System

**Location**: `crates/chatty-core/src/models/write_approval_store.rs`

The write approval flow mirrors the execution approval system for filesystem write operations (file creation, overwrite, delete, move, diff application), and — unlike when this doc was last accurate — it now shares the *same* floating approval bar as execution approvals end to end: `filesystem_write_tool::request_write_approval()` calls the same `notify_approval_via_global()` used by bash approvals, which drives `ChatView::handle_approval_requested()`; resolving it calls `WriteApprovalStore::resolve()` as a fallback in `ChatView::handle_floating_approval()` (`chat_view/handlers.rs`). No new prompt bar needs to be built. What's still missing is a *richer* preview: the bar only shows the flat `description` string passed to `notify_approval_via_global()`, not the structured preview fields below.

| Item | Status | Notes |
|------|--------|-------|
| `WriteApprovalStore::resolve()` | Implemented, called | Fallback branch of `ChatView::handle_floating_approval()` |
| `WriteOperation::is_destructive()` | Implemented, not called | Returns true for delete/overwrite operations |
| `WriteApprovalRequest.id` | Written, not read | Not surfaced beyond the id used to resolve |
| `WriteApprovalRequest.operation` | Written, not read | Would drive a richer, operation-specific prompt |
| `WriteOperation::WriteFile.content_preview` | Written, not read | Preview text for a richer approval dialog |
| `WriteOperation::ApplyDiff.old_preview/new_preview` | Written, not read | Diff preview for a richer approval dialog |

**To wire**: Surface `WriteApprovalRequest.operation` and its preview fields in the floating approval bar (e.g. an operation-specific detail row) instead of the current flat description string.

## Thinking Block Lifecycle (ChatView)

**Location**: `crates/chatty-gpui/src/chatty/views/chat_view/handlers.rs`

Methods for displaying LLM "thinking" / chain-of-thought blocks in the UI. The StreamManager doesn't emit thinking events yet, so ChatView never calls these.

| Method | Line | Purpose |
|--------|------|---------|
| `handle_thinking_started()` | 487 | Initialize a thinking block in the live trace |
| `handle_thinking_delta()` | 559 | Append content to the active thinking block |
| `handle_thinking_ended()` | 570 | Finalize the thinking block |
| `update_thinking_trace()` | 525 | Helper to mutate the active thinking trace |

**To wire**: Add `StreamManagerEvent::ThinkingStarted/Delta/Ended` variants (StreamManager doesn't have them today) and handle them in `handle_stream_manager_event()`.

## Trace Session Methods

**Location**: `crates/chatty-core/src/models/message_types.rs`

| Method | Line | Purpose |
|--------|------|---------|
| `TraceSession::add_thinking()` | 233 | Add a thinking block to the trace |
| `TraceSession::add_approval()` | 252 | Add an approval prompt to the trace |
| `TraceSession::update_approval_state()` | 257 | Update approval state by ID |

**To wire**: `add_approval()`/`update_approval_state()` are already called by the execution/write approval flow above; `add_thinking()` is still called only by the dead thinking block lifecycle methods above.

## Message Event Variants

**Location**: `crates/chatty-core/src/models/message_types.rs`

Event enum variants and their fields that are defined but not matched on:

| Variant / Field | Purpose |
|----------------|---------|
| `ToolCallStateChanged.old_state/new_state` | Track state transitions for animation/logging |
| `ToolCallInputReceived` | Signal when tool call receives its input arguments |
| `ToolCallOutputReceived.has_output` | Signal whether tool produced output |
| `ThinkingStateChanged.old_state/new_state` | Track thinking state transitions |

## Token Budget System

**Location**: `crates/chatty-core/src/token_budget/` and `crates/chatty-gpui/src/chatty/token_budget/`

Several methods in the token budget subsystem are pre-built for planned features:

| Item | File | Purpose |
|------|------|---------|
| `GlobalTokenBudget::publish()` | `chatty-gpui/.../token_budget/manager.rs:63` | Publish a new snapshot to subscribers |
| `GlobalTokenBudget::snapshot()` | `chatty-gpui/.../token_budget/manager.rs:104` | Get current snapshot reference |
| `TokenBudgetSnapshot.computed_at` | `snapshot.rs:10` | Staleness detection in UI |
| `TokenBudgetSnapshot::is_empty()` | `snapshot.rs:138` | Check if snapshot has been computed |
| `ContextStatus::label()` | `snapshot.rs:162` | Human-readable label for popover |
| `ContextPressureEvent` enum | `snapshot.rs:203` | Event for pressure transitions |
| `TokenBudgetCache::invalidate()` | `cache.rs:105` | Clear cache on model switch |
| `TokenBudgetCache::cached_preamble_tokens()` | `cache.rs:125` | Read-through cache accessor |
| `TokenBudgetCache::cached_tool_tokens()` | `cache.rs:131` | Read-through cache accessor |
| `TokenCounter::encoding()` | `counter.rs:87` | Get current encoding for diagnostics |
| `TokenBudgetSummarizer::summarize_with_model()` | `summarizer.rs:140` | Secondary-model summarization (returns error) |
| `PreComputeInput.exec_settings` | `chatty-gpui/.../token_budget/manager.rs:131` | Stored for future tool estimation |
| `PreComputeInput.mcp_server_count` | `chatty-gpui/.../token_budget/manager.rs:133` | Stored for future tool estimation |
| `PreComputeInput.tool_hint` | `chatty-gpui/.../token_budget/manager.rs:142` | Stored for diagnostics |

## Token Tracking Settings

**Location**: `crates/chatty-core/src/settings/models/token_tracking_settings.rs`

| Method | Line | Purpose |
|--------|------|---------|
| `validated()` | 111 | Self-repair after deserialization |
| `should_show_bar()` | 132 | Gate bar rendering on model capability |
| `is_high()` | 138 | Check if utilization crosses high threshold |
| `is_critical()` | 144 | Check if utilization crosses critical threshold |

## Conversation & Store Helpers

| Item | File | Purpose |
|------|------|---------|
| `Conversation::regeneration_records()` | `conversation.rs:468` | Access DPO preference records |
| `ConversationsStore::set_active()` | `conversations_store.rs:203` | Validated active-conversation setter |
| `ConversationsStore::clear_active()` | `conversations_store.rs:219` | Clear active conversation |
| `ConversationsStore::list_recent()` | `conversations_store.rs:232` | Efficient K-recent query |
| `ConversationRepository::load_all()` | `conversation_repository.rs:101` | Load all conversations (trait method; unused — callers use narrower list/metadata queries) |

## Token Usage

**Location**: `crates/chatty-core/src/models/token_usage.rs`

| Method | Purpose |
|--------|---------|
| `TokenUsage::new()` | Constructor |
| `TokenUsage::total_tokens()` | Sum of input + output |
| `ConversationTokenUsage::recalculate_totals()` | Re-derive totals from per-message data |

## Service Utilities

| Item | File | Purpose |
|------|------|---------|
| `ShellSession::shutdown()` | `shell_service/mod.rs:746` | Clean shutdown of bash process |
| `ShellSession::is_running()` | `shell_service/mod.rs:757` | Check process liveness |
| `MathRenderService::clear_cache()` | `math_renderer_service.rs:410` | Clear SVG cache |
| `MathRenderService::cache_size()` | `math_renderer_service.rs:523` | Cache diagnostics |
| `MermaidRenderService::clear_cache()` | `mermaid_renderer_service.rs:357` | Clear rendering cache |
| `MermaidRenderService::cache_size()` | `mermaid_renderer_service.rs:368` | Cache diagnostics |
| `PathValidator::validate_parent()` | `path_validator.rs:158` | Validate paths for glob patterns |
| `is_pdf_extension()` | `attachment_validation.rs:72` | PDF file extension check |

## View Helpers

| Item | File | Purpose |
|------|------|---------|
| `SidebarView::set_collapsed()` | `sidebar_view.rs:79` | Programmatic collapse |
| `CodeBlockComponent::new()` | `code_block_component.rs:34` | Constructor |
| `DisplayMessage::from_assistant_message()` | `message_component.rs:55` | Build display from model |
| `ChattyApp::chat_input_state()` | `app_controller/mod.rs:722` | Access input state entity |
| `AgentClient::provider_name()` | `agent_factory/mod.rs:1031` | Provider name for logging |
| `StreamManager::has_active_streams()` | `stream_manager.rs:588` | Check for active streams |

## Other

| Item | File | Purpose |
|------|------|---------|
| `InstallerError::ExtractionFailed` | `auto_updater/installer.rs:40` | Error variant for extraction failures |
| `StreamStatus` enum | `stream_manager.rs:24` | Stream lifecycle states |
| `StreamManagerEvent` enum | `stream_manager.rs:62` | Stream event variants |
| `ExecutionApprovalRequest` fields (`id`, `command`, `is_sandboxed`, `created_at`) | `execution_approval_store.rs:73-82` | Read by approval UI (fields stored but not all consumed yet) |
