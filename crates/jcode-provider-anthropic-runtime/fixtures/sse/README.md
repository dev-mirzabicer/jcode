# Recorded Claude SSE fixtures

Raw streaming responses captured on 2026-09-29 (INT-01/WP-02) with
`jcode provider-doctor claude --contract claude-oauth --model claude-opus-5-5
--capture-sse <dir>`. The route was Claude OAuth through the native Anthropic
runtime, using the production request builder with effort `high` and the binding
control (`thinking.block_binding`) that production sends.

| File | Request |
|---|---|
| `claude-opus-5-5-thinking-summarized.sse` | The contract probe's thinking task with `display: "summarized"` |
| `claude-opus-5-5-thinking-omitted.sse` | The same task with the default `display: "omitted"`: the signed block has empty text |
| `claude-opus-5-5-thinking-follow-up.sse` | The next turn. The summarized turn is replayed as streamed, then a tool result and a follow-up, under `prefix_mismatch_behavior: "error"`; the provider accepted it |

The files hold only what the provider streamed. Signatures are opaque provider
data kept for byte-exact round-trip tests; they carry no credential. The tests in
`src/anthropic_tests.rs` compose the multi-block and `redacted_thinking` cases
from these payloads and label them synthetic, because these models returned one
thinking block per response and never return `redacted_thinking`.
