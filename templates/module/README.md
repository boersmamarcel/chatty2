# {{project-name}}

{{description}}

## Prerequisites

```sh
rustup target add wasm32-wasip2
```

## Build

```sh
cargo build --release
```

The compiled WASM component is at
`target/wasm32-wasip2/release/{{project-name | snake_case}}.wasm`.

## Test

```sh
cargo test
```

## Deploy

Copy the `.wasm` file and `module.toml` into your chatty modules folder and
start (or restart) the chatty registry. The agent will be available at:

- `POST /v1/{{project-name}}/chat/completions` — OpenAI-compatible
- `POST /mcp/{{project-name}}` — MCP JSON-RPC
- `POST /a2a/{{project-name}}` — A2A JSON-RPC

## License

Proprietary — see [LICENSE](LICENSE).
