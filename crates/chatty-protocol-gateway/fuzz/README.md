# Fuzzing the worker socket's frame codec

`FrameCodec` reads every line a worker or the broker receives, so it is the
first thing untrusted input reaches (ADR-0021 § 1; EN-0b added the target,
EN-1 moved it to the codec). The target feeds each input's lines, in order,
to a broker codec and a worker codec that have already exchanged a hello and
a `task.run`, so the id state is live. A line must decode, be dropped or
fail with a `FrameError`, never panic; a frame one side decodes must encode
on the other side and decode back.

Needs a nightly toolchain and `cargo-fuzz`:

```sh
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz
cd crates/chatty-protocol-gateway
cargo +nightly fuzz run frame-codec -- -max_total_time=60
```

The corpus and any crash artifacts land in `fuzz/corpus/` and
`fuzz/artifacts/`, which are not committed.
