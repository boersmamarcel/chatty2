Diagnose LLM response streaming issues in the Chatty application.

Investigate the following areas based on the user's description of the problem:

## Stream Lifecycle (StreamManager)
- Check `src/chatty/models/stream_manager.rs` for stream state management
- Verify the stream is registered, promoted from pending if needed, and properly finalized
- Check that `StreamManagerEvent` variants are handled in `handle_stream_manager_event()`
- Verify cancellation tokens (`Arc<AtomicBool>`) are checked at the top of each stream loop iteration

## Agent Factory
- Check `src/chatty/factories/agent_factory.rs` for agent construction
- Verify provider-specific client setup (API keys, base URLs, model identifiers)
- Check tool registration and MCP tool attachment

## LLM Service
- Check `src/chatty/services/llm_service.rs` for stream processing
- Verify the `process_agent_stream!` macro handles all `StreamItem` variants
- Check that `StreamChunk::Done` is yielded at stream end

## Common Issues
- Stream never starts: Check if conversation creation or agent building fails
- Stream hangs: Look for missing `StreamChunk::Done` or infinite loops
- Partial responses: Check cancellation flag or network timeout
- Wrong model: Verify `ModelConfig.model_identifier` matches provider expectations
- Missing tools: Check `execution_settings` and tool enablement flags

Report findings with specific file paths and line numbers.
