# Changelog

All notable changes to hive-billing-sdk will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
