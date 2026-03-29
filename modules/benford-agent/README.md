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

## Protocol comparison

The benford-agent exposes the **same underlying logic** through three different
protocol surfaces. Each surface is optimised for a different caller. Use this
section as a concrete test to see all three in action.

> **Test dataset** — all three examples use the same 9 invoice amounts:
> `1234 4521 891 2340 567 8901 234 456 789`

---

### 1 · Completion API (`/v1/{module}/chat/completions`)

**What it does**: Triggers the full agentic loop (LLM → tools → LLM → report)
and wraps the final audit report in an OpenAI-compatible response. Best for
any client that already speaks the OpenAI API.

```sh
curl -X POST http://localhost:8420/v1/benford-agent/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "benford-agent",
    "messages": [{
      "role": "user",
      "content": "Analyze these invoice amounts: 1234 4521 891 2340 567 8901 234 456 789"
    }]
  }'
```

**Expected response shape**:

```json
{
  "id": "chatcmpl-...",
  "object": "chat.completion",
  "model": "benford-agent",
  "choices": [{
    "index": 0,
    "message": {
      "role": "assistant",
      "content": "## Benford's Law Audit Report\n\n**Risk level: LOW** (χ² = 4.21, df = 8, p ≥ 0.05)\n\nThe distribution of leading digits across the 9 invoice amounts conforms to Benford's Law. No statistically significant anomaly was detected. ..."
    },
    "finish_reason": "stop"
  }],
  "usage": { "prompt_tokens": 412, "completion_tokens": 198, "total_tokens": 610 }
}
```

The `content` field is the LLM-synthesised narrative report. The intermediate
tool calls (`compute_benford_distribution`, `chi_square_test`) are invisible to
the caller — they happen inside the WASM agentic loop.

---

### 2 · MCP (`/mcp/{module}`)

**What it does**: Exposes the two tools as individual callable functions via
JSON-RPC 2.0. There is **no** agentic loop — the MCP client (typically another
LLM or orchestrator) decides when and how to call each tool. Best for
integrating the Benford analysis primitives into a larger MCP toolchain.

#### Step A — discover available tools

```sh
curl -X POST http://localhost:8420/mcp/benford-agent \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}'
```

**Expected response**:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "tools": [
      {
        "name": "compute_benford_distribution",
        "description": "Compute the first-digit frequency distribution ...",
        "inputSchema": {
          "type": "object",
          "properties": {
            "numbers": { "type": "array", "items": { "type": "number" } }
          },
          "required": ["numbers"]
        }
      },
      {
        "name": "chi_square_test",
        "description": "Run a chi-square goodness-of-fit test ...",
        "inputSchema": {
          "type": "object",
          "properties": {
            "observed_counts": { "type": "array", "items": { "type": "integer" } },
            "total": { "type": "integer" }
          },
          "required": ["observed_counts", "total"]
        }
      }
    ]
  }
}
```

#### Step B — call `compute_benford_distribution` directly

```sh
curl -X POST http://localhost:8420/mcp/benford-agent \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 2,
    "method": "tools/call",
    "params": {
      "name": "compute_benford_distribution",
      "arguments": {
        "numbers": [1234, 4521, 891, 2340, 567, 8901, 234, 456, 789]
      }
    }
  }'
```

**Expected response** (raw JSON from the WASM tool — no LLM involved):

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "content": [{
      "type": "text",
      "text": "{\"total_analyzed\":9,\"observed_counts\":[2,2,0,2,1,0,0,1,1],\"distribution\":[{\"digit\":1,\"observed_count\":2,\"observed_pct\":22.22,\"expected_pct\":30.10,\"deviation\":-7.88},{\"digit\":2,\"observed_count\":2,\"observed_pct\":22.22,\"expected_pct\":17.61,\"deviation\":4.61},...],}"
    }]
  }
}
```

#### Step C — call `chi_square_test` with the counts from step B

```sh
curl -X POST http://localhost:8420/mcp/benford-agent \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 3,
    "method": "tools/call",
    "params": {
      "name": "chi_square_test",
      "arguments": {
        "observed_counts": [2, 2, 0, 2, 1, 0, 0, 1, 1],
        "total": 9
      }
    }
  }'
```

**Expected response**:

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "result": {
    "content": [{
      "type": "text",
      "text": "{\"chi_square\":4.210,\"degrees_of_freedom\":8,\"risk_level\":\"LOW\",\"most_deviant_digit\":1,\"interpretation\":\"Distribution conforms to Benford's Law. No statistically significant anomaly detected.\"}"
    }]
  }
}
```

With MCP you get the raw statistical output — no synthesised narrative. Your
orchestrator or the host LLM writes the interpretation.

---

### 3 · A2A (`/a2a/{module}`)

**What it does**: Triggers the full agentic loop (identical to the Completion
API path) but speaks the Agent-to-Agent JSON-RPC 2.0 protocol. The response
wraps the audit report in an A2A `message` with typed `parts`. Best for
agent-to-agent communication where both sides understand A2A (LangGraph,
CrewAI, ADK, other chatty instances).

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

**Expected response shape**:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "message": {
      "role": "agent",
      "parts": [{
        "type": "text",
        "text": "## Benford's Law Audit Report\n\n**Risk level: LOW** (χ² = 4.21, df = 8, p ≥ 0.05)\n\nThe distribution of leading digits across the 9 invoice amounts conforms to Benford's Law. ..."
      }]
    }
  }
}
```

The narrative content is the same as the Completion API response — only the
envelope differs (`choices[0].message.content` vs `result.message.parts[0].text`).

---

### Side-by-side summary

| | **Completion API** | **MCP** | **A2A** |
|---|---|---|---|
| **Endpoint** | `POST /v1/benford-agent/chat/completions` | `POST /mcp/benford-agent` | `POST /a2a/benford-agent` |
| **Protocol** | OpenAI chat completions | JSON-RPC 2.0 `tools/list`, `tools/call` | JSON-RPC 2.0 `message/send` |
| **Agentic loop** | ✅ Full (LLM → tools → LLM) | ❌ No — raw tool calls only | ✅ Full (LLM → tools → LLM) |
| **Response envelope** | `choices[0].message.content` | `result.content[0].text` (raw JSON) | `result.message.parts[0].text` |
| **Response content** | Narrative audit report | Raw statistical output | Narrative audit report |
| **Caller controls LLM?** | No — WASM drives it | Yes — caller orchestrates | No — WASM drives it |
| **Ideal for** | OpenAI-compatible clients, chatty UI | MCP orchestrators, tool-using agents | A2A ecosystem (LangGraph, CrewAI, ADK, other chatty) |
| **Discover agent?** | `GET /.well-known/agent.json` | `tools/list` | `GET /.well-known/agent.json` |

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
