# Debug streams & rendering

**When to read this:** Investigate stuck streams, missing text, or layout bugs.

## Stream lifecycle

1. Read [stream-manager.md](../architecture/stream-manager.md)
2. Trace: `message_ops` → `StreamManager.register` → async loop → `StreamManagerEvent`
3. Cancellation uses `Arc<AtomicBool>` — not task drop

## GPUI rendering

1. Enable overlay: `export CHATTY_DEBUG_UI=1` before launching `chatty`
2. Read [debug_ui.md](../architecture/debug_ui.md)
3. Chat view code: `chatty-gpui/src/chatty/views/chat_view/`

## Common issues

| Symptom | Check |
|---------|-------|
| Stream never ends | StreamManager status, cancel flag |
| Text duplicated | Conversation model vs ChatView state |
| Tool UI stuck | ToolCallStarted/Finished events |
| Whitespace overlap | Rendering pipeline, markdown cache |
