Review the code changes in the current branch for correctness, architecture compliance, and potential issues.

Perform these checks on the diff (use `git diff main...HEAD` or staged changes):

## 1. Architecture Compliance
- Entity-to-entity communication uses `EventEmitter`/`cx.subscribe()`, never `Arc<dyn Fn>` callbacks between entities
- Global state uses `cx.set_global()`/`cx.global()` with `WeakEntity` for entity references
- Async operations use `cx.spawn()` with proper `AsyncApp` context
- StreamManager owns all stream state; no stream fields scattered in other entities
- Cancellation uses `Arc<AtomicBool>` flags, not task drops

## 2. Error Handling
- No silent `.ok()` — all discarded errors should log with `warn!()` first
- File I/O and network failures propagate with `?`
- Non-critical UI refresh failures log as warnings

## 3. Security
- MCP env vars sent to the LLM use `masked_env()`, never raw `.env`
- `MASKED_VALUE_SENTINEL` ("****") is preserved correctly in edit operations
- No sensitive values in log statements (log key names only)
- Filesystem operations validate paths against workspace sandboxing
- Fetch tool checks for SSRF (no private IPs)

## 4. GPUI Patterns
- `.detach()` called on all subscriptions
- `cx.notify()` called after entity state mutations
- `cx.defer()` used to avoid re-entrant entity updates
- Closures clone entity references before the `move` closure

## 5. General
- No over-engineering or unnecessary abstractions
- No backwards-compatibility hacks for removed code
- Complex functions (>100 lines) have phase documentation
- No unused imports or dead code

Provide actionable feedback with file paths and line numbers.
