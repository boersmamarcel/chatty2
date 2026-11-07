
## 2026-11-07 - step 2
Non-SSE message/stream reply with a JSON-RPC error or failed/canceled task now yields one failed final StatusUpdate (no artifact); SSE block with error and no result yields Err (new parse_sse_error). Not built or tested: cargo deliberately not run.
