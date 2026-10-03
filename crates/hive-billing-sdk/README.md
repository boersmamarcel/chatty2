# hive-billing-sdk

Publisher-facing Rust SDK for integrating Hive billing into WASM modules.

## Overview

This SDK wraps the WIT `billing` interface with ergonomic helpers for paid modules on the Hive marketplace. It provides:

- **Session acquisition** with cryptographic verification
- **Session token verification** (EdDSA, certified by the registry root key)
- **Usage reporting** for post-execution settlement
- **Helper functions** like `require_session()` and `report_usage()`

## Installation

Add to your module's `Cargo.toml`:

```toml
[dependencies]
chatty-module-sdk = { path = "../../crates/chatty-module-sdk" }
hive-billing-sdk = { path = "../../crates/hive-billing-sdk" }
```

## Quick Start

The SDK is used inside a plugin's `invoke_tool` (a `chatty:plugin@0.3.0`
plugin built on `chatty-module-sdk`). It uses `chatty-module-sdk`'s own
`billing` imports, so the two crates link into one component.

```rust
use chatty_module_sdk::*;
use hive_billing_sdk::{require_session, report_usage};

struct MyPaidPlugin;

impl Plugin for MyPaidPlugin {
    fn metadata() -> PluginMetadata {
        PluginMetadata {
            name: "my-paid-plugin".into(),
            version: "0.1.0".into(),
            description: "Summarises text, billed per token".into(),
            requested_capabilities: vec![Capability::Llm, Capability::Billing],
            config_keys: vec![],
        }
    }

    fn list_tools() -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            name: "summarise".into(),
            description: "Summarise the input text".into(),
            parameters_schema: r#"{"type":"object","properties":{"input":{"type":"string"}}}"#.into(),
        }]
    }

    fn invoke_tool(call: ToolCallRequest) -> Result<ToolResult, ToolError> {
        // 1. Acquire and verify a billing session (reserve 5000 tokens)
        let session = require_session(5000).map_err(ToolError::denied)?;

        // 2. Do the work
        let prompt = Message::new(Role::User, call.arguments_json);
        let response = llm::complete("", &[prompt], None).map_err(ToolError::failed)?;

        // 3. Report the actual usage
        let usage = response.usage.unwrap_or(TokenUsage { input_tokens: 0, output_tokens: 0 });
        report_usage(&session, usage.input_tokens.into(), usage.output_tokens.into())
            .map_err(ToolError::failed)?;

        Ok(ToolResult { content: response.content, usage: Some(usage) })
    }
}

export!(MyPaidPlugin);
```

## Trust Model

The session token is an **EdDSA** (Ed25519) JWT signed by hive-registry's
session key. Every token carries:

- `skey`: the session public key that signed it (hex)
- `skey_sig`: the registry root's base64 signature certifying that key, over
  `hive-registry/session-key-cert/v1\n` followed by the key's hex

`require_session()` verifies, in order:

1. `skey_sig` under one of the trusted registry root public keys
2. the token's EdDSA signature under `skey`
3. that the token has not expired, and that it reserves what was asked

Any other algorithm is refused. Only public keys are embedded in the module,
so there is nothing secret in the binary. Tokens are short-lived (5 minutes).

`verify_session_token(token, roots, now)` is public if you need to check a
token you obtained some other way.

## Configuration

A module built for the production registry needs no configuration. It trusts
`PRODUCTION_ROOT_PUBLIC_KEYS`, which is compiled in and mirrors hive-client's
SEC-3 list. That list is empty until the real root key is pinned, so until
then every token is refused unless you name a root key yourself.

To test against a local registry (for example the hive docker compose stack),
pass its root public key (hex Ed25519) once, at module initialisation and
before `require_session()`:

```rust
hive_billing_sdk::configure_root_keys(&[env!("HIVE_DEV_ROOT_PUBLIC_KEY")]);
```

`configure_root_keys` replaces the production list and panics if called twice.

## API Reference

### `configure_root_keys(keys: &[&str])`

Trust these hex Ed25519 registry root public keys instead of
`PRODUCTION_ROOT_PUBLIC_KEYS`. For a local registry only. Call once before
`require_session()`; panics if called twice.

### `verify_session_token(token: &str, roots: &[String], now: i64) -> Result<SessionClaims, String>`

Verify a token against `roots` at `now` (Unix seconds): root certifies `skey`,
`skey` signed the token (EdDSA), and the token has not expired.

### `require_session(estimated_tokens: i64) -> Result<BillingSession, String>`

Acquire and verify a billing session:
1. Calls the host's `billing::acquire-session` import
2. Verifies the token with `verify_session_token` under the trusted root keys
3. Checks that the reserved tokens match the request
4. Returns a verified `BillingSession`

**Returns:** 
- `Ok(BillingSession)` on success
- `Err(String)` if verification fails, insufficient credits, or no root key is trusted

### `report_usage(session: &BillingSession, input_tokens: i64, output_tokens: i64) -> Result<(), String>`

Report actual token usage after work completes. Settles the session:
- Deducts actual usage from user's balance
- Releases over-reserved tokens

### `report_usage_simple(input_tokens: i64, output_tokens: i64) -> Result<(), String>`

Simplified version that doesn't require passing the session object.

## Types

### `BillingSession`

```rust
pub struct BillingSession {
    /// The raw session token (JWT)
    pub token: String,
    /// Verified claims from the token
    pub claims: SessionClaims,
    /// User's balance at session creation
    pub balance_tokens: i64,
    /// Tokens reserved for this session
    pub reserved_tokens: i64,
    /// Pricing model ("free" or "paid")
    pub pricing_model: String,
}
```

### `SessionClaims`

```rust
pub struct SessionClaims {
    pub sid: String,           // Session ID (UUID)
    pub uid: String,           // User ID (UUID)
    pub module_name: String,   // Module name
    pub ver: String,           // Module version
    pub res: i64,              // Reserved tokens
    pub bal: i64,              // User balance
    pub iat: i64,              // Issued at (Unix timestamp)
    pub exp: i64,              // Expires at (Unix timestamp)
    pub skey: String,          // Session public key that signed the token (hex)
    pub skey_sig: String,      // Root's base64 signature certifying skey
}
```

## Error Handling

All functions return `Result<T, String>` for easy integration with WIT exports.

Common errors:
- `"no registry root key is trusted: ..."` — No root key is pinned yet; call `configure_root_keys()` for a local registry
- `"Session token verification failed"` — Uncertified session key, bad signature, or expired token
- `"Insufficient credits"` — User balance too low
- `"Session token mismatch"` — Reserved tokens don't match request

## Testing

Since this targets `wasm32-wasip2`, tests must run in a WASM runtime with the billing host imports available.

For development, test in the context of a full chatty module build and runtime.

## Security Best Practices

1. **Fail closed**: Return errors if billing verification fails; never skip checks
2. **Over-estimate**: Reserve more tokens than expected to avoid mid-execution failures
3. **Report accurately**: Under-reporting costs users more (full reservation deducted after timeout)
4. **Use remote execution for high-value modules**: Phase 4 Firecracker hosting eliminates local trust issues

## Related Documentation

- [Billing Trust Model Design Doc](https://github.com/boersmamarcel/hive/blob/main/design-docs/v0.2/10-billing-trust-model.md)
- [Hive Issue #71](https://github.com/boersmamarcel/hive/issues/71) — Phase 3b Billing Implementation
- [chatty-wasm-runtime](../../crates/chatty-wasm-runtime) — Host-side billing support

## License

Same as the parent chatty2 project.
