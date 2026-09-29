# Fuzzing the worker socket's frame decoder

`decode_frame` reads every line a worker or the broker receives, so it is
the first thing untrusted input reaches (ADR-0021, EN-0b). The target decodes
arbitrary input as both a `ParticipantFrame` and a `BrokerFrame`; a frame
that decodes must encode and decode back to the same encoding.

Needs a nightly toolchain and `cargo-fuzz`:

```sh
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz
cd crates/chatty-protocol-gateway
cargo +nightly fuzz run decode-frame -- -max_total_time=60
```

The corpus and any crash artifacts land in `fuzz/corpus/` and
`fuzz/artifacts/`, which are not committed. ADR-0021 step 1 (EN-1) moves this
target to `FrameCodec`.
