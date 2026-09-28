# benford

Benford's Law plugin (`chatty:plugin@0.3.0`): the two tools of a forensic
Benford audit. The auditing agent is a spec, not this module: the
`benford-analyst` preset (`crates/chatty-core/agents/benford-analyst.toml`)
is chatty's own harness with a forensic-auditor preamble and this plugin.

**Tutorial:** [give an agent the plugin](https://boersmamarcel.github.io/chatty2/dev/start/tutorial-benford-agent.html)
(mdBook) · full source in this directory.

---

## What it does

| Tool | Input | Output |
|------|-------|--------|
| `compute_benford_distribution` | `numbers: [f64]` | observed vs expected first-digit frequencies, signed deviation per digit, `observed_counts` array, `total_analyzed` |
| `chi_square_test` | `observed_counts: [u64]` (9 values), `total: u64` | χ² statistic, degrees of freedom, **risk level** (`LOW` / `MEDIUM` / `HIGH`), most deviant digit, plain-English interpretation |

Both run deterministically in pure Rust inside the WASM sandbox and request
no capability. Bad arguments come back as an `invalid-arguments` tool error
the model can read.

Risk thresholds (df = 8):

| χ² statistic | Risk   | p-value |
|--------------|--------|---------|
| > 20.090     | HIGH   | < 0.01  |
| > 15.507     | MEDIUM | < 0.05  |
| ≤ 15.507     | LOW    | ≥ 0.05  |

Benford's Law: in naturally occurring financial data the leading digit
follows a logarithmic distribution (~30.1 % start with 1, ~17.6 % with 2, …
~4.6 % with 9). Significant deviations can indicate fraud, entry errors or
fabrication.

---

## Usage

### As the `benford-analyst` agent

```sh
chatty-tui --agent benford-analyst --headless \
  -m "Analyze these invoice amounts: 1234 4521 891 2340 567 8901 234 456 789"
```

The agent's model calls `benford__compute_benford_distribution`, then
`benford__chi_square_test` with the counts it got back, and writes the audit
report. For that dataset the plugin answers χ² ≈ 10.49, risk `LOW`
(`crates/chatty-tui/tests/plugins_headless.rs` checks exactly that against a
scripted model). The preset finds the plugin in the module directory
(Settings → Modules), so install it there first (see [Build](#build)).

### In your own agent

```toml
[agent]
name = "my-auditor"
preamble = "Audit the numbers you are given with the benford tools."

[[plugins]]
module = "benford"
version = "^0.2"
```

### Over MCP, for an external client

The protocol gateway serves the plugin's tools at `POST /mcp/benford`
(`[protocols] mcp = true` in `module.toml`):

```sh
curl -X POST http://localhost:8420/mcp/benford \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call",
       "params":{"name":"compute_benford_distribution",
                 "arguments":{"numbers":[1234,4521,891,2340,567,8901,234,456,789]}}}'
```

There is no OpenAI or A2A route for a plugin: it has tools, not a loop.

---

## Build

```sh
rustup target add wasm32-wasip2
cd modules/benford
cargo build --target wasm32-wasip2 --release
cp target/wasm32-wasip2/release/benford.wasm .
cp -r ../benford ~/.local/share/chatty/modules/   # the module directory
```

### Run unit tests (on host)

This crate's `.cargo/config.toml` defaults the target to `wasm32-wasip2`, so a
bare `cargo test` tries to *execute* the `.wasm`; pass the host target:

```sh
cd modules/benford
cargo test --target x86_64-unknown-linux-gnu   # or your host's triple
```

---

## Project layout

```
modules/benford/
├── Cargo.toml              # cdylib, standalone [workspace], serde_json dep
├── .cargo/config.toml      # default target = wasm32-wasip2
├── module.toml             # registry manifest (name, wasm path, mcp = true, …)
├── src/
│   └── lib.rs              # the Plugin impl + tool functions + tests
└── README.md               # this file
```
