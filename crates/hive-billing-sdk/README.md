# hive-billing-sdk

Publisher-facing Rust SDK for integrating Hive billing into WASM modules.

## Overview

This SDK wraps the WIT `billing` interface with ergonomic helpers for paid modules on the Hive marketplace. It provides:

- **Session acquisition** with cryptographic verification
- **JWT token verification** using Hive's signing key
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
use hive_billing_sdk::{configure_secret, require_session, report_usage};

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
        // 1. Configure the JWT secret (once per instance)
        configure_secret(env!("HIVE_JWT_SECRET"));

        // 2. Acquire and verify a billing session (reserve 5000 tokens)
        let session = require_session(5000).map_err(ToolError::denied)?;

        // 3. Do the work
        let prompt = Message::new(Role::User, call.arguments_json);
        let response = llm::complete("", &[prompt], None).map_err(ToolError::failed)?;

        // 4. Report the actual usage
        let usage = response.usage.unwrap_or(TokenUsage { input_tokens: 0, output_tokens: 0 });
        report_usage(&session, usage.input_tokens.into(), usage.output_tokens.into())
            .map_err(ToolError::failed)?;

        Ok(ToolResult { content: response.content, usage: Some(usage) })
    }
}

export!(MyPaidPlugin);
```

## Trust Model

### Current Implementation: HMAC-SHA256

The current implementation uses **HS256** (HMAC-SHA256) signing with a shared secret. This provides **deterrence-level security**:

✅ **Prevents casual tampering**: Harder than modifying the host runtime
✅ **Verifiable by module**: Cryptographic proof that Hive issued the token
✅ **Short-lived tokens**: 5-minute expiry limits abuse window

⚠️ **Not cryptographically perfect**: A determined attacker can extract the secret from WASM
⚠️ **Shared secret**: Module must embed Hive's JWT secret (rotatable but present in binary)

### Why This Works for Phase 3b

The design goal is **"not worth the effort"**, not **"mathematically impossible"**:

1. **Honest users** (~99%) never encounter friction
2. **Attackers** must reverse-engineer WASM, extract secrets, and maintain a patched runtime
3. **Cost-benefit**: Paying for usage is easier than building/maintaining a crack

### Future: Ed25519 Upgrade

A future release will migrate to Ed25519 asymmetric cryptography:
- Hive signs JWTs with a private key (never shared)
- Modules verify using Hive's public key (safe to embed)
- True public-key verification

The API remains unchanged — only internal verification evolves.

## Configuration

### Option 1: Compile-Time Secret

Set the secret at compile time via environment variable:

```bash
export HIVE_JWT_SECRET="your-hive-jwt-secret"
cargo build --target wasm32-wasip2
```

Then in your module:

```rust
configure_secret(env!("HIVE_JWT_SECRET"));
```

### Option 2: Runtime Configuration (TODO)

Fetch from a well-known Hive endpoint at module load:

```rust
// Future: HTTP fetch for wasm32-wasip2
// let secret = fetch_hive_secret().await?;
// configure_secret(secret);
```

## API Reference

### `configure_secret(secret: impl Into<String>)`

Configure the Hive JWT secret for token verification. Must be called once before `require_session()`.

### `require_session(estimated_tokens: i64) -> Result<BillingSession, String>`

Acquire and verify a billing session:
1. Calls the host's `billing::acquire-session` import
2. Verifies the returned JWT signature
3. Validates token expiry and claims
4. Returns a verified `BillingSession`

**Returns:** 
- `Ok(BillingSession)` on success
- `Err(String)` if verification fails, insufficient credits, or not configured

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
}
```

## Error Handling

All functions return `Result<T, String>` for easy integration with WIT exports.

Common errors:
- `"Billing SDK not configured"` — Call `configure_secret()` first
- `"Session token verification failed"` — Invalid signature or expired token
- `"Insufficient credits"` — User balance too low
- `"Session token mismatch"` — Reserved tokens don't match request

## Testing

Since this targets `wasm32-wasip2`, tests must run in a WASM runtime with the billing host imports available.

For development, test in the context of a full chatty module build and runtime.

## Security Best Practices

1. **Rotate secrets regularly**: Update `HIVE_JWT_SECRET` and republish modules
2. **Fail closed**: Return errors if billing verification fails; never skip checks
3. **Over-estimate**: Reserve more tokens than expected to avoid mid-execution failures
4. **Report accurately**: Under-reporting costs users more (full reservation deducted after timeout)
5. **Use remote execution for high-value modules**: Phase 4 Firecracker hosting eliminates local trust issues

## Related Documentation

- [Billing Trust Model Design Doc](https://github.com/boersmamarcel/hive/blob/main/design-docs/v0.2/10-billing-trust-model.md)
- [Hive Issue #71](https://github.com/boersmamarcel/hive/issues/71) — Phase 3b Billing Implementation
- [chatty-wasm-runtime](../../crates/chatty-wasm-runtime) — Host-side billing support

## License

Same as the parent chatty2 project.
