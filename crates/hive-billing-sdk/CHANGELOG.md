# Changelog

All notable changes to hive-billing-sdk will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed
- Billing session tokens are now EdDSA (Ed25519) JWTs signed by
  hive-registry's session key (CX-0b, AGE-823). The SDK verifies the token's
  `skey_sig` under the trusted registry root public keys
  (`PRODUCTION_ROOT_PUBLIC_KEYS`, or `configure_root_keys()` for a local
  registry), then the token signature under `skey`, then expiry.
- `verify_session_token(token, roots, now)` is public.
- Targets `chatty:plugin@0.3.0` and calls `chatty-module-sdk`'s `billing`
  imports instead of generating its own bindings of the world, so a plugin
  can depend on both crates (two generated worlds collided at link time).

### Removed
- `configure_secret()` and HS256 verification. EdDSA verification under the
  registry root key list replaced them.

## [0.1.0] - 2026-04-21

### Added
- Initial release of hive-billing-sdk
- `require_session()` function for acquiring and verifying billing sessions
- `report_usage()` function for settling token usage
- JWT signature verification using HMAC-SHA256 (pure Rust, WASM-compatible)
- Session token verification with expiry checking
- `configure_secret()` for JWT secret configuration
- Comprehensive README with examples and security considerations
- Integration tests for JWT verification
- Full compatibility with hive-registry JWT format (HS256)

### Security
- Pure Rust JWT implementation (no native dependencies, WASM-safe)
- Fails closed: returns errors if verification fails
- Token expiry validation
- HMAC-SHA256 signature verification

### Notes
- Targets wasm32-wasip2
- Designed for Phase 3b of Hive billing trust model
- Compatible with chatty-module-sdk v0.1.0
- Uses WIT bindings from chatty:module@0.2.0
