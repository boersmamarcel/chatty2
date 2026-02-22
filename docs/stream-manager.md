# StreamManager Architecture

The StreamManager is a centralized GPUI entity that manages stream lifecycle (status, cancellation, token usage, trace), emits typed events for decoupled UI updates, and uses cancellation tokens for graceful shutdown. It enables concurrent multi-conversation streaming where background streams continue accumulating data even when the UI is showing a different conversation.

**Key design principle:** StreamManager does NOT accumulate response text. Text accumulation is the sole responsibility of `ConversationsStore.streaming_message`, ensuring a single source of truth and avoiding dual-write divergence.

## Entity Ownership

```
GlobalStreamManager (GPUI Global)
  └── Entity<StreamManager>
        └── HashMap<String, StreamState>   (one entry per active stream)
              ├── status: StreamStatus
              ├── token_usage: Option<(u32, u32)>
              ├── trace_json: Option<Value>
              ├── task: Option<Task>
              └── cancel_flag: Arc<AtomicBool>

ConversationsStore (GPUI Global)
  └── HashMap<String, Conversation>
        ├── history: Vec<Message>
        ├── streaming_message: Option<String>   ← single source of truth for streaming text
        ├── agent: AgentClient
        ├── model_id, title, token_usage, ...
        └── system_traces: Vec<Option<Value>>

ChattyApp (GPUI Entity, window root)
  ├── Entity<ChatView>
  │     ├── messages: Vec<DisplayMessage>
  │     ├── conversation_id: Option<String>   ← which conversation the UI is showing
  │     ├── pending_approval: Option<PendingApprovalInfo>
  │     └── Entity<ChatInputState>
  │           ├── is_streaming: bool
  │           ├── selected_model_id, attachments
  │           └── emits: ChatInputEvent (Send, ModelChanged, Stop)
  └── Entity<SidebarView>
        ├── conversations list
        └── emits: SidebarEvent (NewChat, OpenSettings, SelectConversation, ...)
```

## Event Subscription Chain

```mermaid
graph TD
    SM[StreamManager] -->|cx.emit StreamManagerEvent| CA[ChattyApp]
    CA -->|handle_stream_manager_event| CV[ChatView]
    CV -->|creates/updates| STV[SystemTraceView]
    STV -->|cx.emit TraceEvent| CV

    CIS[ChatInputState] -->|cx.emit ChatInputEvent| CA
    SB[SidebarView] -->|cx.emit SidebarEvent| CA
    MN[McpNotifier] -->|cx.emit McpNotifierEvent| CA
```

All entity-to-entity communication uses `EventEmitter`/`cx.subscribe()`. The 4 subscriptions are set up in `ChattyApp::setup_callbacks()`:

```rust
cx.subscribe(&manager, |app, _mgr, event: &StreamManagerEvent, cx| {
    app.handle_stream_manager_event(event, cx);
}).detach();
```

## StreamManagerEvent Variants

| Event | Emitted by | Handler action |
|-------|-----------|----------------|
| `StreamStarted` | `register_stream`, `register_pending_stream` | Sets `ChatInputState.is_streaming = true` (deferred) |
| `TextChunk` | `handle_chunk` | `ChatView.append_assistant_text()` |
| `ToolCallStarted` | `handle_chunk` | `ChatView.handle_tool_call_started()` |
| `ToolCallInput` | `handle_chunk` | `ChatView.handle_tool_call_input()` |
| `ToolCallResult` | `handle_chunk` | `ChatView.handle_tool_call_result()` |
| `ToolCallError` | `handle_chunk` | `ChatView.handle_tool_call_error()` |
| `ApprovalRequested` | `handle_chunk` | `ChatView.handle_approval_requested()` |
| `ApprovalResolved` | `handle_chunk` | `ChatView.handle_approval_resolved()` |
| `TokenUsage` | `handle_chunk` | No-op (processed during finalization) |
| `StreamEnded` | `finalize_stream`, `stop_stream`, `cancel_pending`, `stop_all` | Resets streaming state; dispatches to `finalize_completed_stream` or `finalize_stopped_stream`; clears `Conversation.streaming_message` |

All events carry a `conversation_id`. The handler checks `view.conversation_id() == Some(conversation_id)` before forwarding to ChatView -- events for non-displayed conversations are silently skipped at the UI level, while data-level operations (finalize, persist) always execute.

## Text Accumulation: Single Source of Truth

During streaming, text is accumulated in **one** location only:

```
StreamChunk::Text("hello")
    │
    ├──► ConversationsStore: conv.append_streaming_content("hello")
    │    Single source of truth for streaming text.
    │    - Used for background stream restoration when switching conversations
    │    - Read at finalization to save the complete response to history
    │
    └──► StreamManager: handle_chunk() emits TextChunk event (pass-through only)
         StreamManager does NOT store the text. It only forwards the event
         to the UI subscription for real-time display.
```

At finalization, `finalize_completed_stream` / `finalize_stopped_stream` reads the accumulated text from `Conversation.streaming_message`, calls `conv.finalize_response()` to move it into history, then clears `streaming_message`.

This design avoids dual-write divergence where two copies of the same text could fall out of sync due to independent error handling paths.

## Sequence Diagrams

### Send message (new conversation)

```mermaid
sequenceDiagram
    participant U as User
    participant CIS as ChatInputState
    participant CA as ChattyApp
    participant SM as StreamManager
    participant CS as ConversationsStore
    participant CV as ChatView
    participant LLM as LLM API

    U->>CIS: Press Enter
    CIS->>CA: cx.emit(ChatInputEvent::Send)
    CA->>CS: active_id() → None

    Note over CA: Create cancel_flag, resolved_id=Arc<Mutex<None>>

    CA->>CA: cx.spawn(async task)
    CA->>SM: register_pending_stream(task, resolved_id, cancel_flag)
    SM-->>CA: StreamStarted { "__pending__" }
    CA->>CIS: set_streaming(true)

    Note over CA: Inside async task:
    CA->>CA: create_new_conversation()
    CA->>CS: Add new Conversation
    CA->>CA: Update resolved_id → real conv_id
    CA->>SM: promote_pending(conv_id)
    CA->>CV: add_user_message()
    CA->>CV: start_assistant_message()
    CA->>LLM: stream_prompt()

    loop Each chunk from LLM
        LLM-->>CA: StreamChunk::Text
        CA->>CS: conv.append_streaming_content(text)
        CA->>SM: handle_chunk(conv_id, chunk)
        SM-->>CA: TextChunk { conv_id, text }
        CA->>CV: append_assistant_text(text)
    end

    LLM-->>CA: StreamChunk::Done
    CA->>CV: extract_current_trace()
    CA->>SM: set_trace() + finalize_stream()
    SM-->>CA: StreamEnded { Completed }
    CA->>CIS: set_streaming(false)
    CA->>CV: finalize_assistant_message()
    CA->>CS: Read streaming_message, finalize_response()
    CA->>CA: Generate title, calculate cost, persist
```

### Send message (existing conversation)

```mermaid
sequenceDiagram
    participant U as User
    participant CIS as ChatInputState
    participant CA as ChattyApp
    participant SM as StreamManager
    participant CS as ConversationsStore
    participant CV as ChatView
    participant LLM as LLM API

    U->>CIS: Press Enter
    CIS->>CA: cx.emit(ChatInputEvent::Send)
    CA->>CS: active_id() → Some(conv_id)

    Note over CA: Create cancel_flag

    CA->>CA: cx.spawn(async task)
    CA->>SM: register_stream(conv_id, task, cancel_flag)
    SM-->>CA: StreamStarted { conv_id }
    CA->>CIS: set_streaming(true)

    Note over CA: Inside async task:
    CA->>CS: conv.add_user_message_with_attachments()
    CA->>CV: add_user_message()
    CA->>CV: start_assistant_message()
    CA->>LLM: stream_prompt()

    loop Each chunk from LLM
        LLM-->>CA: StreamChunk::Text
        CA->>CS: conv.append_streaming_content(text)
        CA->>SM: handle_chunk(conv_id, chunk)
        SM-->>CA: TextChunk { conv_id, text }
        CA->>CV: append_assistant_text(text)
    end

    LLM-->>CA: StreamChunk::Done
    CA->>SM: finalize_stream()
    SM-->>CA: StreamEnded { Completed }
    CA->>CIS: set_streaming(false)
    CA->>CA: finalize_completed_stream()
```

### Switch conversation during active stream

```mermaid
sequenceDiagram
    participant U as User
    participant SB as SidebarView
    participant CA as ChattyApp
    participant SM as StreamManager
    participant CS as ConversationsStore
    participant CV as ChatView
    participant BG as Background Stream (conv A)

    Note over CV: Currently showing conv A (streaming)

    U->>SB: Click conversation B
    SB->>CA: cx.emit(SidebarEvent::SelectConversation("B"))
    CA->>CA: load_conversation("B")
    CA->>CS: set_active("B")
    CA->>SM: is_streaming("B")? → false
    CA->>CV: set_conversation_id("B")
    CA->>CV: load_history(B's messages)

    Note over CV: Now showing conv B (not streaming)

    par Conv A continues in background
        BG->>CS: conv_A.append_streaming_content(text)
        BG->>SM: handle_chunk("A", TextChunk)
        SM-->>CA: TextChunk { conv_id: "A" }
        CA->>CV: view.conversation_id() == "B" ≠ "A"
        Note over CA: Event silently skipped (UI filter)
    end

    Note over U: User switches back to A

    U->>SB: Click conversation A
    SB->>CA: cx.emit(SidebarEvent::SelectConversation("A"))
    CA->>CA: load_conversation("A")
    CA->>CS: set_active("A"), get streaming_message
    CA->>SM: is_streaming("A")? → true
    CA->>CV: set_conversation_id("A")
    CA->>CV: load_history(A's messages)
    CA->>CIS: set_streaming(true)
    CA->>CV: start_assistant_message()
    CA->>CV: append_assistant_text(accumulated_content)

    Note over CV: Restored! New chunks now match conv_id and continue
```

### Stop stream

```mermaid
sequenceDiagram
    participant U as User
    participant CIS as ChatInputState
    participant CA as ChattyApp
    participant SM as StreamManager
    participant CS as ConversationsStore
    participant CV as ChatView

    U->>CIS: Click Stop
    CIS->>CA: cx.emit(ChatInputEvent::Stop) → stop_stream()
    CA->>CS: active_id() → conv_id
    CA->>CV: extract_current_trace()
    CA->>SM: set_trace(conv_id, trace_json)
    CA->>SM: stop_stream(conv_id)

    Note over SM: Sets cancel_flag = true<br/>Sets status = Cancelled<br/>Drops task (backstop)

    SM-->>CA: StreamEnded { conv_id, Cancelled }
    CA->>CIS: set_streaming(false)
    CA->>CA: finalize_stopped_stream()
    CA->>CV: mark_message_cancelled()
    CA->>CS: Read streaming_message, finalize_response(partial_text)
    CA->>CS: conv.set_streaming_message(None)
    CA->>CA: persist_conversation()
```

### Cancel pending (New Chat while stream starting)

```mermaid
sequenceDiagram
    participant U as User
    participant SB as SidebarView
    participant CA as ChattyApp
    participant SM as StreamManager
    participant CIS as ChatInputState

    Note over SM: __pending__ stream exists

    U->>SB: Click New Chat
    SB->>CA: cx.emit(SidebarEvent::NewChat)
    CA->>SM: cancel_pending()

    Note over SM: Sets cancel_flag for __pending__<br/>Emits StreamEnded { "__pending__", Cancelled }

    SM-->>CA: StreamEnded { "__pending__", Cancelled }
    CA->>CIS: set_streaming(false)

    Note over CA: Skip finalize_stopped_stream<br/>(no real conversation exists)

    CA->>CA: create_new_conversation()
```

## Cancellation Mechanism

StreamManager uses `Arc<AtomicBool>` cancellation tokens rather than dropping tasks:

```
cancel_flag = Arc<AtomicBool::new(false)>
    │
    ├── Shared with stream loop (cancel_flag_for_loop)
    │   Checked at top of each iteration:
    │     if cancel_flag_for_loop.load(Relaxed) { break; }
    │
    └── Owned by StreamState
        Set by stop_stream / cancel_pending:
          state.cancel_flag.store(true, Relaxed);
```

The stream exits cleanly on the next iteration rather than being abruptly terminated mid-chunk. The task `drop()` in `stop_stream` is a backstop in case the loop doesn't check the flag in time.

## Pending Stream Promotion

When sending a message creates a new conversation, there's a window where the stream starts before the conversation ID is known:

```
1. register_pending_stream()     → stored under "__pending__" key
2. Async: create_new_conversation() → returns real conv_id
3. promote_pending(conv_id)      → moves entry from "__pending__" to conv_id
```

The `pending_resolved_ids` map tracks the `Arc<Mutex<Option<String>>>` so that `stop_stream` and `is_streaming` can match a pending stream to its resolved conversation ID even before `promote_pending` is called.

## Lifecycle: Init and Shutdown

**Init** (`main.rs`): StreamManager is created as a GPUI entity and stored as a **strong** `Entity<StreamManager>` reference in `GlobalStreamManager`. Using a strong reference (not `WeakEntity`) prevents garbage collection after the initialization closure returns.

**Shutdown** (Quit action): Calls `StreamManager.stop_all()` which iterates all active streams, sets their cancel flags, emits `StreamEnded` for each, and clears the HashMap.
