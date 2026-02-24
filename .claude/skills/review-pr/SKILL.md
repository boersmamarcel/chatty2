---
name: review-pr
description: Reviews pull request changes against Chatty's architectural patterns and coding standards documented in CLAUDE.md. Use when reviewing PRs, checking code quality, or validating that changes follow project conventions.
argument-hint: "[pr-number]"
allowed-tools: Bash, Read, Grep, Glob
---

# Review PR

Reviews a pull request for adherence to Chatty's architecture, patterns, and coding standards.

## Getting PR Context

If a PR number is provided as `$ARGUMENTS`:

```bash
gh pr diff $0
gh pr view $0 --json title,body,files
```

Otherwise, review the current branch's diff against `master`:

```bash
git diff master...HEAD
```

## Review Checklist

Evaluate the changes against each applicable category:

### 1. Global Entity Patterns
- Are globals initialized properly with `cx.set_global()`?
- Are entity references stored as `WeakEntity<T>` in globals (not strong references)?
- Is `cx.has_global()` checked before assuming a global exists?

### 2. Event-Subscribe Patterns
- Do subscriptions call `.detach()`?
- Is entity-to-entity communication done via `EventEmitter`/`cx.subscribe()` (not `Arc<dyn Fn>` callbacks)?
- Are events defined as enums implementing `EventEmitter`?

### 3. Async Patterns
- Are long-running operations in `cx.spawn()` blocks?
- Is `cx.update()` used inside `AsyncApp` context for UI access?
- Are Tasks returned from methods that callers need to await?

### 4. Error Handling
- No silent `.ok()` calls — errors must be logged with `warn!()` or propagated with `?`
- File I/O and critical operations use `?` propagation
- UI refresh failures use `.map_err(|e| warn!(...)).ok()`

### 5. Stream Lifecycle
- Are streams managed through `StreamManager`?
- Is cancellation done via `Arc<AtomicBool>` flags (not task drops)?
- Do stream loops check the cancel flag at the top of each iteration?

### 6. Security
- MCP server env vars sent to LLM use `masked_env()` not `.env`
- No API keys or secrets in log statements
- New LLM-facing output structs exclude sensitive fields

### 7. View Rendering
- Is `cx.notify()` called after mutating entity state?
- Are closure captures properly cloned before move closures?
- Is `cx.defer()` used when updating an entity that's currently being updated?

### 8. Model Capabilities
- New providers have `default_capabilities()` entries in `ProviderType`
- Capability checks use `ModelConfig` fields (not hardcoded provider checks)

### 9. General Code Quality
- No over-engineering or unnecessary abstractions
- No backwards-compatibility hacks (unused `_vars`, re-exports of removed types)
- Complex functions (>100 lines) have phase documentation
- Optimistic update pattern used where applicable (instant UI, async persist)

## Output Format

Provide the review as:

1. **Summary**: One-line overall assessment
2. **Issues**: List of specific problems found, with file paths and line numbers
3. **Suggestions**: Optional improvements that aren't blocking
4. **Verdict**: APPROVE, REQUEST_CHANGES, or COMMENT
