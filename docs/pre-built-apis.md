# Pre-Built APIs

This document catalogs public APIs that are defined but not yet wired to callers. They are marked with `#[allow(dead_code)]` in the source and exist as scaffolding for planned features. Contributors should wire these APIs rather than reimplementing the functionality.

## Write Approval System

**Location**: `src/chatty/models/write_approval_store.rs`

The write approval flow mirrors the execution approval system but for filesystem write operations (file creation, overwrite, delete, move, diff application). The store and types are fully implemented; what's missing is the UI for presenting write approval prompts.

| Item | Status | Notes |
|------|--------|-------|
| `WriteApprovalStore::resolve()` | Implemented, not called | Resolves a pending write approval by ID |
| `WriteOperation::is_destructive()` | Implemented, not called | Returns true for delete/overwrite operations |
| `WriteApprovalRequest.id` | Written, not read | Will be displayed in approval UI |
| `WriteApprovalRequest.operation` | Written, not read | Will be displayed in approval UI |
| `WriteOperation::WriteFile.content_preview` | Written, not read | Preview text for the approval dialog |
| `WriteOperation::ApplyDiff.old_preview/new_preview` | Written, not read | Diff preview for the approval dialog |

**To wire**: Create a write approval prompt bar (similar to `ApprovalPromptBar`) and subscribe to `WriteApprovalStore` pending requests in `ChattyApp`.

## Thinking Block Lifecycle (ChatView)

**Location**: `src/chatty/views/chat_view.rs`

Methods for displaying LLM "thinking" / chain-of-thought blocks in the UI. The StreamManager emits thinking events but ChatView doesn't yet process them.

| Method | Line | Purpose |
|--------|------|---------|
| `handle_thinking_started()` | 697 | Initialize a thinking block in the live trace |
| `handle_thinking_delta()` | 773 | Append content to the active thinking block |
| `handle_thinking_ended()` | 784 | Finalize the thinking block |
| `update_thinking_trace()` | 735 | Helper to mutate the active thinking trace |

**To wire**: Add `StreamManagerEvent::ThinkingStarted/Delta/Ended` handling in `handle_stream_manager_event()`.

## Trace Session Methods

**Location**: `src/chatty/views/message_types.rs`

| Method | Line | Purpose |
|--------|------|---------|
| `TraceSession::add_thinking()` | 181 | Add a thinking block to the trace |
| `TraceSession::add_approval()` | 201 | Add an approval prompt to the trace |
| `TraceSession::update_approval_state()` | 207 | Update approval state by ID |

**To wire**: Called by the thinking block lifecycle methods above and the write approval UI.

## Message Event Variants

**Location**: `src/chatty/views/message_types.rs`

Event enum variants and their fields that are defined but not matched on:

| Variant / Field | Purpose |
|----------------|---------|
| `ToolCallStateChanged.old_state/new_state` | Track state transitions for animation/logging |
| `ToolCallInputReceived` | Signal when tool call receives its input arguments |
| `ToolCallOutputReceived.has_output` | Signal whether tool produced output |
| `ThinkingStateChanged.old_state/new_state` | Track thinking state transitions |

## Token Budget System

**Location**: `src/chatty/token_budget/`

Several methods in the token budget subsystem are pre-built for planned features:

| Item | File | Purpose |
|------|------|---------|
| `GlobalTokenBudget::publish()` | `manager.rs:62` | Publish a new snapshot to subscribers |
| `GlobalTokenBudget::snapshot()` | `manager.rs:103` | Get current snapshot reference |
| `TokenBudgetSnapshot.computed_at` | `snapshot.rs:9` | Staleness detection in UI |
| `TokenBudgetSnapshot::is_empty()` | `snapshot.rs:137` | Check if snapshot has been computed |
| `ContextStatus::label()` | `snapshot.rs:161` | Human-readable label for popover |
| `ContextPressureEvent` enum | `snapshot.rs:201` | Event for pressure transitions |
| `TokenBudgetCache::invalidate()` | `cache.rs:104` | Clear cache on model switch |
| `TokenBudgetCache::cached_preamble_tokens()` | `cache.rs:124` | Read-through cache accessor |
| `TokenBudgetCache::cached_tool_tokens()` | `cache.rs:130` | Read-through cache accessor |
| `TokenCounter::encoding()` | `counter.rs:79` | Get current encoding for diagnostics |
| `TokenBudgetSummarizer::summarize_with_model()` | `summarizer.rs:142` | Secondary-model summarization (panics) |
| `PreComputeInput.exec_settings` | `manager.rs:130` | Stored for future tool estimation |
| `PreComputeInput.mcp_server_count` | `manager.rs:132` | Stored for future tool estimation |
| `PreComputeInput.tool_hint` | `manager.rs:141` | Stored for diagnostics |

## Token Tracking Settings

**Location**: `src/settings/models/token_tracking_settings.rs`

| Method | Line | Purpose |
|--------|------|---------|
| `validated()` | 113 | Self-repair after deserialization |
| `should_show_bar()` | 134 | Gate bar rendering on model capability |
| `is_high()` | 140 | Check if utilization crosses high threshold |
| `is_critical()` | 146 | Check if utilization crosses critical threshold |

## Conversation & Store Helpers

| Item | File | Purpose |
|------|------|---------|
| `Conversation::message_timestamps()` | `conversation.rs:405` | Access per-message timestamps |
| `Conversation::regeneration_records()` | `conversation.rs:446` | Access DPO preference records |
| `ConversationsStore::set_active()` | `conversations_store.rs:135` | Validated active-conversation setter |
| `ConversationsStore::clear_active()` | `conversations_store.rs:151` | Clear active conversation |
| `ConversationsStore::list_recent()` | `conversations_store.rs:164` | Efficient K-recent query |
| `ConversationRepository::load_all()` | `conversation_repository.rs:86` | Load all conversations |

## Token Usage

**Location**: `src/chatty/models/token_usage.rs`

| Method | Purpose |
|--------|---------|
| `TokenUsage::new()` | Constructor |
| `TokenUsage::total_tokens()` | Sum of input + output |
| `ConversationTokenUsage::recalculate_totals()` | Re-derive totals from per-message data |

## Service Utilities

| Item | File | Purpose |
|------|------|---------|
| `ShellSession::shutdown()` | `shell_service.rs:674` | Clean shutdown of bash process |
| `ShellSession::is_running()` | `shell_service.rs:685` | Check process liveness |
| `MathRenderService::clear_cache()` | `math_renderer_service.rs:373` | Clear SVG cache |
| `MathRenderService::cache_size()` | `math_renderer_service.rs:483` | Cache diagnostics |
| `MermaidRenderService::clear_cache()` | `mermaid_renderer_service.rs:252` | Clear rendering cache |
| `MermaidRenderService::cache_size()` | `mermaid_renderer_service.rs:260` | Cache diagnostics |
| `PathValidator::validate_parent()` | `path_validator.rs:157` | Validate paths for glob patterns |
| `is_pdf_extension()` | `attachment_validation.rs:68` | PDF file extension check |

## View Helpers

| Item | File | Purpose |
|------|------|---------|
| `SidebarView::set_collapsed()` | `sidebar_view.rs:75` | Programmatic collapse |
| `CodeBlockComponent::new()` | `code_block_component.rs:20` | Constructor |
| `DisplayMessage::from_assistant_message()` | `message_component.rs:200` | Build display from model |
| `ChattyApp::chat_input_state()` | `app_controller.rs:2335` | Access input state entity |
| `AgentClient::provider_name()` | `agent_factory.rs:1108` | Provider name for logging |
| `StreamManager::has_active_streams()` | `stream_manager.rs:516` | Check for active streams |

## Other

| Item | File | Purpose |
|------|------|---------|
| `InstallerError::ExtractionFailed` | `auto_updater/installer.rs:39` | Error variant for extraction failures |
| `ListToolsError` enum | `tools/list_tools_tool.rs:26` | Error type for list tools |
| `StreamStatus` enum | `stream_manager.rs:19` | Stream lifecycle states |
| `StreamManagerEvent` enum | `stream_manager.rs:54` | Stream event variants |
| `ExecutionApprovalRequest` fields (`id`, `command`, `is_sandboxed`, `created_at`) | `execution_approval_store.rs:72-81` | Read by approval UI (fields stored but not all consumed yet) |
