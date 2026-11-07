
## 2026-11-07 - step 2
Non-SSE message/stream reply with a JSON-RPC error or failed/canceled task now yields one failed final StatusUpdate (no artifact); SSE block with error and no result yields Err (new parse_sse_error). Not built or tested: cargo deliberately not run.

## 2026-11-07 - step 3
Added 3 tests in a2a_client.rs mod tests: plain_json_failed_task_is_one_failed_status, plain_json_rpc_error_is_a_failed_status, sse_error_block_without_result_is_an_err (helper stream_items). rustfmt --check clean. Not compiled or run: cargo deliberately not run.
