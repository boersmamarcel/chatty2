# AGE-1110 / AGE-1394 item a: MCP gateway stops reporting usage

## Acceptance criteria
1. `rg -w 'record_usage|CallUsage|call_usage' crates/chatty-protocol-gateway` finds nothing.
2. `cargo test -p chatty-protocol-gateway` passes; no test removed except one asserting a usage report.
3. A free module call still runs through tools/call; a paid module is still refused by admit on a meter without credit guard.
4. Only module_call.rs, mcp.rs, gateway.rs change (plus .chatty state files).

## Steps
1. [x] remove record_usage/call_usage/import from module_call.rs
2. [x] remove call in mcp.rs and unused vars
3. [x] update comments in gateway.rs and module comment
4. [x] run checks (see progress.md for results)
