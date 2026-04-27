# {{project-name}}

{{description}}

## Build

```sh
# Install the WASM target (one time)
rustup target add wasm32-wasip2

# Build
cargo build --target wasm32-wasip2 --release

# Copy the WASM to the module directory
cp target/wasm32-wasip2/release/{{project-name | snake_case}}.wasm .
```

## Usage

Copy this directory into your chatty modules folder and start (or restart)
the chatty registry. The agent will be available at:

- `POST /v1/{{project-name}}/chat/completions` — OpenAI-compatible
- `POST /mcp/{{project-name}}` — MCP JSON-RPC
- `POST /a2a/{{project-name}}` — A2A JSON-RPC

## Customise

Edit `src/lib.rs` to implement your agent logic. See the
[echo-agent](../../modules/echo-agent/README.md) for a full example.
