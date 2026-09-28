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

A plugin contributes tools to an agent; it is never an agent itself. Copy
this directory into your chatty modules folder (Settings → Modules → module
directory, by default `~/.local/share/chatty/modules/` on Linux), then list it
in an agent spec, e.g. `.chatty/agents/my-agent.toml`:

```toml
[agent]
name = "my-agent"
preamble = "Use the greet tool to greet people."

[[plugins]]
module = "{{project-name}}"
```

`chatty-tui --agent my-agent` then offers the model the tool as
`{{project-name}}__greet`. External MCP clients reach the same tools at
`POST /mcp/{{project-name}}` on the protocol gateway.

## Customise

Edit `src/lib.rs` to add tools. See the [echo plugin](../../modules/echo/README.md)
for a full example.
