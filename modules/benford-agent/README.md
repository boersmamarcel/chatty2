# benford-agent

Forensic Benford's Law audit agent — a chatty WASM module that demonstrates a
**full agentic tool-calling loop** running entirely inside the WASM sandbox.

---

## What it does

Given a list of financial numbers, the agent autonomously:

1. Calls the host LLM with two tool definitions
2. The LLM requests `compute_benford_distribution` → executed locally in WASM
3. The LLM requests `chi_square_test` with the returned counts → executed locally
4. Tool results are fed back into the conversation
5. The LLM synthesises a professional audit report with risk rating and recommendations

This is a genuine agentic loop: the module drives multi-turn LLM ↔ tool-call
exchanges without any host-side orchestration.

---

## Benford's Law background

Benford's Law states that in naturally-occurring financial datasets the leading
digit follows a logarithmic distribution: ~30% of numbers start with 1, ~17.6%
with 2, and so on. Significant deviations from this pattern can indicate fraud,
data entry errors, or fabrication.

| Digit | Expected % |
|------:|----------:|
| 1     | 30.1 %     |
| 2     | 17.6 %     |
| 3     | 12.5 %     |
| 4     |  9.7 %     |
| 5     |  7.9 %     |
| 6     |  6.7 %     |
| 7     |  5.8 %     |
| 8     |  5.1 %     |
| 9     |  4.6 %     |

---

## Tools

| Tool | Input | Output |
|------|-------|--------|
| `compute_benford_distribution` | `numbers: [f64]` | observed vs expected first-digit frequencies, signed deviation per digit, `observed_counts` array, `total` |
| `chi_square_test` | `observed_counts: [u64]`, `total: u64` | χ² statistic, degrees of freedom, **risk level** (`LOW` / `MEDIUM` / `HIGH`), most deviant digit, plain-English interpretation |

Risk thresholds (df = 8):

| χ² statistic | Risk   | p-value  |
|-------------|--------|---------|
| > 20.090    | HIGH   | < 0.01  |
| > 15.507    | MEDIUM | < 0.05  |
| ≤ 15.507    | LOW    | ≥ 0.05  |

---

## Usage

### Via chatty `/agent` command (local module)

```text
/agent benford-agent Analyze these invoice amounts: 1234 4521 891 2340 567 8901 234 456 789
```

### Via A2A HTTP (when loaded in the protocol gateway)

The protocol gateway automatically exposes every loaded module at
`/a2a/{module-name}`. Send a `message/send` JSON-RPC request:

```sh
curl -X POST http://localhost:8420/a2a/benford-agent \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "message/send",
    "params": {
      "message": {
        "parts": [{
          "type": "text",
          "text": "Analyze these invoice amounts: 1234 4521 891 2340 567 8901 234 456 789"
        }]
      }
    }
  }'
```

### Via A2A from chatty (Settings → A2A Agents)

Add an agent entry in **Settings → A2A Agents**:

- **Name**: `benford-agent`
- **URL**: `http://localhost:8420/a2a/benford-agent`

Then call it from the chat:

```text
/agent benford-agent Analyze these invoice amounts: 1234 4521 891 2340 567 8901
```

### Via MCP (tools exposed to the main agent)

When loaded and enabled as an MCP server, the two tools
(`compute_benford_distribution`, `chi_square_test`) become available to the
main chatty agent automatically.

---

## Build

### Prerequisites

```sh
# Rust toolchain (stable)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# WASM target
rustup target add wasm32-wasip2
```

### Compile

```sh
cd modules/benford-agent
cargo build --target wasm32-wasip2 --release

# Copy the WASM binary to the module directory
cp target/wasm32-wasip2/release/benford_agent.wasm .
```

### Run unit tests (on host)

The pure-Rust tool implementations have full unit test coverage and can be run
on the host without the WASM target:

```sh
cd modules/benford-agent
cargo test
```

### Load into chatty

Copy the compiled directory into your chatty modules folder:

```sh
cp -r modules/benford-agent ~/.local/share/chatty/modules/
```

Then go to **Settings → Modules** and enable `benford-agent`.

---

## Project layout

```
modules/benford-agent/
├── Cargo.toml              # cdylib, standalone [workspace], serde_json dep
├── .cargo/config.toml      # default target = wasm32-wasip2
├── module.toml             # registry manifest (name, wasm path, a2a = true, …)
├── src/
│   └── lib.rs              # BenfordAgent impl + tool functions + tests
└── README.md               # this file
```

---

## How the agentic loop works

```
chat(req)
  │
  ├── Build messages: [system, user_prompt]
  │
  └── Loop (up to 6 turns):
        │
        ├── llm::complete(messages, tools=TOOLS_JSON)
        │       │
        │       ├── tool_calls present?
        │       │     YES → invoke_tool() locally for each call
        │       │           → append [assistant tool-call msg, user tool-result msg]
        │       │           → continue loop
        │       │
        │       └── tool_calls empty?
        │             YES → return ChatResponse { content: audit_report }
        │
        └── (fallback) max turns → ask LLM to summarise
```

The tools run deterministically in pure Rust inside the WASM sandbox — no
network calls, no external dependencies beyond `serde_json` for argument parsing.
