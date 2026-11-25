# Recorded chatty-server responses (AGE-2083, HX-4)

What a `chatty-server`'s `/api` routes answer, as files:

| File | Route |
|:-----|:------|
| `conversations_page.json` | `GET /api/conversations` (a page: `conversations`, `next_cursor`) |
| `conversation_detail.json` | `GET /api/conversations/{id}` (history with tool-call messages) |
| `turn.sse` | `POST /api/conversations/{id}/turns` |
| `turn_events_reattach.sse` | `GET /api/conversations/{id}/turn/events` (`turn_attached` first) |
| `broker_approval.sse` | a stream with `broker_approval` then `broker_input_settled` |
| `connector_needs_sign_in.sse` | a stream with `connector_needs_sign_in` |
| `stream_error.sse` | a stream with `stream_error` |

They are replayed by `chatty-core`'s `session/hosted_recorded.rs` against
`HostedSession` (decoding through `HostedFrame`).

**Who may rewrite them.** Once hive's nightly job lands, only that job
(`HIVE_E2E_RECORD=1`) may rewrite these files, the same rule as the registry
recordings in the parent directory. Never edit one by hand.

Until then the generator is the source: `UPDATE_GOLDENS=1 cargo test -p
chatty-core --lib session::hosted_recorded::record_fixtures` serializes the
real `SessionEvent` and `ApprovalRequest` values, so the payloads cannot drift
from the Rust types. The `event:` name of a session event is its variant in
snake_case; the client decodes the `data:` and ignores the name.
