---
name: review
description: Review code changes for Chatty-specific patterns, GPUI idioms, and common pitfalls. Use for code review, PR review, or checking code quality.
disable-model-invocation: true
allowed-tools: Bash, Read, Grep, Glob
argument-hint: [file-or-branch]
---

# Code Review for Chatty

Review code changes with focus on Chatty-specific patterns and GPUI idioms.

## Scope

If `$ARGUMENTS` is a file path, review that file. If it's a branch name, review changes on that branch vs main. If empty, review uncommitted changes via `git diff`.

## Review Checklist

### 1. GPUI Patterns
- [ ] `cx.notify()` called after state mutations
- [ ] Subscriptions are `.detach()`ed
- [ ] `WeakEntity` used in globals (not strong references)
- [ ] `cx.defer()` used to avoid re-entrancy
- [ ] Entity references cloned before closures
- [ ] Render methods use fluent API consistently

### 2. Async Patterns
- [ ] `cx.spawn()` used for async operations (not raw tokio::spawn)
- [ ] `AsyncApp` context uses `cx.update()` for UI access
- [ ] Error handling: no silent `.ok()` — use `.map_err(|e| warn!(...)).ok()`
- [ ] Tasks `.detach()`ed or awaited

### 3. Entity Communication
- [ ] Entity-to-entity communication via `EventEmitter`/`cx.subscribe()` (no `Arc<dyn Fn>` callbacks between entities)
- [ ] `IntoElement` components can use callbacks but should route through parent `cx.emit()`
- [ ] Events are descriptive enums, not stringly-typed

### 4. State Management
- [ ] Global state accessed via `cx.global()`/`cx.update_global()`
- [ ] Optimistic updates: immediate UI update, then async persistence
- [ ] No redundant state — single source of truth

### 5. Security
- [ ] Sensitive env vars use `masked_env()` before LLM exposure
- [ ] `MASKED_VALUE_SENTINEL` ("****") preserved on edit, rejected on add
- [ ] Filesystem tools respect workspace boundaries
- [ ] No secrets logged (log key names only)
- [ ] No command injection vulnerabilities in bash/shell execution

### 6. Stream Management
- [ ] Streams managed via `StreamManager` entity
- [ ] Cancellation uses `Arc<AtomicBool>` flags (not task drops)
- [ ] Stream loop only updates Conversation model + StreamManager
- [ ] UI updates via `StreamManagerEvent` (not direct chat_view calls from stream loop)

### 7. General Rust
- [ ] No unnecessary `.clone()` — prefer references where possible
- [ ] Error types are informative
- [ ] No `unwrap()` in production code (use `?`, `expect()`, or match)
- [ ] Clippy-clean: `cargo clippy -- -D warnings`

## Output

Provide findings organized by severity:
1. **Critical** — bugs, security issues, memory leaks
2. **Important** — pattern violations, maintainability concerns
3. **Suggestions** — style, minor improvements

Include file paths and line numbers for each finding.
