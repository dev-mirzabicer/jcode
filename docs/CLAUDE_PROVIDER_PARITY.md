# Claude provider parity

When jcode drives a Claude model over Claude OAuth or the Anthropic API key, the
model sees the same system prompt, tools, transcript and dynamic context as a
GPT model over OpenAI OAuth. Its signed thinking is replayed, and the prompt
cache follows the conversation. This page describes that behavior and who owns
each part. The evidence and its history are in the
[acceptance ledger](dev/CLAUDE_OAUTH_PARITY_ACCEPTANCE.md).

Two differences are unavoidable. Claude OAuth requests start `system` with two
identity blocks (the billing header and "You are a Claude agent, built on
Anthropic's Claude Agent SDK."), as the official Agent SDK does with a custom
system prompt. And each provider uses its own dialect for the same meaning:
the tool-schema dialect, `is_error` instead of `[Error]` text, signed
`thinking` blocks instead of encrypted reasoning items.

The routes covered are native Anthropic over OAuth and over an API key. The
deprecated Claude CLI subprocess route does not have these properties; see
[Claude CLI route](#claude-cli-route). Bedrock, Vertex, OpenRouter and Copilot
Claude routes are not covered.

## What a Claude request contains

In render order:

| Part | Content |
|---|---|
| `tools` | Every registry tool, under its registry name, with its registry description and the Anthropic dialect of its schema. There are no provider-authored stand-ins. `tool_choice` is `{"type": "auto", "disable_parallel_tool_use": true}`, matching GPT's `parallel_tool_calls: false`; jcode's `batch` tool is the parallelism mechanism. `strict` is not sent (the Anthropic API rejects the shared strict schemas). |
| `system` | On OAuth, the two identity blocks, then the static system prompt: the composed instructions, plus the active skill and the dormant Swarm effort directive when present. Nothing changes per request. |
| `messages` | The projected transcript (context control applies), with signed thinking replayed exactly as stored. Per-turn system reminders, the batch nudge and reload continuations are ordinary persisted transcript content, identical for GPT ([delivery contract](NOTIFICATIONS.md#delivery-of-model-visible-context)). |

Between recorded transitions each request is the previous one with content
appended (append-only history). The recorded transitions are: a context-control
apply, revert or reapply; clear, rewind or a provider or model switch; a
static-prompt transition (skill activation, Swarm effort directive, instruction
reload or agent replacement); and a tool-set transition (below).

## Tool-set lifetime

Tools render first in every provider prefix, and Claude binds each thinking
block to the tool set. So a session's tool set is locked at its first request
and changes only at a named tool-set transition:

| Transition | When |
|---|---|
| Late MCP registration | MCP servers connect in the background so the first turn is never blocked. Tools that register after the set was locked join it once, when they appear. A later wave waits for the `mcp` tool. |
| MCP tool set reload | The `mcp` tool connected, disconnected or reloaded servers. The next request rebuilds the set. |
| Tool unavailable | A tool in the set became globally unavailable (Swarm disabled). |

Each one is journaled as a cache transition and named as the cause of any
thinking it invalidates. Changes to history (context-control transitions,
rewind and its undo, tool-output repair) and provider or model switches keep
the set: the tool surface depends on neither. Clear and a session change start
a new provider history and lock a new set at their first request. The agent
loops and the TUI local loop share this rule (`jcode_app_core::tool::tool_set`).

## Thinking

- **Capture.** Each `thinking` and `redacted_thinking` block is stored per
  block, in stream order, byte-exact, including empty-text signed blocks, with
  the producing model and a binding: a digest of the prefix it was produced
  under and its predecessor block.
- **Replay.** Top-level Claude sessions and roster-routed children replay stored
  thinking unchanged and in place. Blocks stored before bindings were recorded
  (before INT-01/WP-02) are never replayed. Earlier top-level turns stored
  thinking as history-only traces, so replay starts with the first turn after
  the update.
- **Binding.** Opus 5.5, Sonnet 5.5 and Fable 5.1 bind thinking to the
  conversation before it. When a change leaves a replayed block bound to a
  prefix the next request no longer has, jcode suppresses it explicitly,
  persisted and visible, before sending ([context control](CONTEXT_CONTROL.md#replayed-reasoning-bound-to-its-prefix)).
  Which models bind is policy as data: `anthropic_reasoning_binding` in
  `jcode-provider-core`, one evidence-cited entry per model.
- **Safety net.** Requests to binding models carry
  `thinking.block_binding.prefix_mismatch_behavior: "drop_block"` (beta
  `thinking-binding-controls-2026-08-01`). A drop the API reports in
  `input_transformations` is logged as a defect and raises a notice. Setting
  `JCODE_ANTHROPIC_PREFIX_MISMATCH=error` makes a mismatch fail the request
  instead; verification runs use it.
- **Display.** Requests with thinking ask for `display: "summarized"`, which
  returns reasoning summaries and the progress notes the model writes between
  tool calls. `"updates"` would return only the progress notes, so it is not
  used.

## Reasoning effort and sampling

Per-model data in `jcode-provider-core` (`anthropic_reasoning_caps`):

| Model | Default effort when none is configured | Effort `none` |
|---|---|---|
| Opus 5.5 | `medium` (the API default) | not offered; means `low` (thinking cannot be disabled) |
| Opus 5 | `low` | `thinking: {type: "disabled"}` |
| Opus 4.7, 4.8 | `xhigh` | thinking omitted |
| Opus 4.5, 4.6 | `high` | thinking omitted |
| Fable 5, 5.1 | `high` | not offered; means `low` |
| Sonnet 5.5 | model default | not offered; means `low` |
| Sonnet 5 | model default | `thinking: {type: "disabled"}` |
| Sonnet 4.x, Haiku, Mythos, unknown generations | model default | as the generation allows |

Sonnet 5.5's `{type: "between_tools"}` is not used for `none`: it rejects
`block_binding`, which would remove the safety net. `temperature` is sent only
on the OAuth route without thinking, and only to models that still accept
sampling parameters (Opus before 4.7, Sonnet before 5, Haiku 4.5).

## Prompt cache

`jcode_provider_anthropic::place_cache_breakpoints` places every marker on the
built request:

1. the last `system` block (it covers the tools rendered before it), or the
   last tool when there is no system;
2. the last block of the newest message, written for the next request;
3. the last block of the user message before the latest assistant response,
   where the previous request wrote, so each request reads exactly the
   previous entry;
4. when more than 15 lookback positions separate 3 and 2, one more marker 15
   positions before the newest (the API looks back 20 positions; a run of tool
   calls or of tool results is one position).

Thinking blocks never carry a marker. Every marker uses the configured TTL,
one hour by default (`set_cache_ttl_1h`). A person's pause between turns often
exceeds five minutes, which would expire a five-minute entry and write the
whole conversation again; the one-hour write premium applies only to the
content each request appends. The measured cadence behind this choice is in
the ledger.

## Models

Opus 5.5, Sonnet 5.5 and Fable 5.1 are in the curated model list
(`ALL_CLAUDE_MODELS`) and API pricing, with their 1M context window and 128K
output. `claude-opus-5` remains the default model.

## Claude CLI route

The deprecated Claude CLI subprocess sends only the latest prompt and relies on
the CLI's own `--resume` state. None of the properties above hold there, and
context-control operations are disabled. It is used only when no native
Anthropic provider is configured, and each provider instance says so once, at
its first request: "Claude CLI route: jcode's Claude parity (the registry tool
surface, thinking replay, append-only context and cache placement) does not
apply to this deprecated subprocess transport. Use the native Anthropic
provider (`--provider claude`) for it."

## Checking a route

- `jcode provider-doctor claude --contract claude-oauth --model <id>` sends the
  production request shapes to the live API and compares each outcome with the
  recorded contract ([ledger](dev/CLAUDE_OAUTH_PARITY_ACCEPTANCE.md#live-provider-contract)).
  It spends subscription quota.
- `CACHE_INVALIDATION_DOCUMENTED` log lines name every recorded transition;
  `INV-1` warnings name a replayed block found invalid at request time.

## Ownership

| Concern | Owner |
|---|---|
| Tool dialect, parallel policy, cache placement, binding digests | `jcode-provider-anthropic` |
| Request building, stream parsing, safety net, effort resolution | `jcode-provider-anthropic-runtime` |
| Per-model capabilities and policy (binding, thinking off, sampling, default effort), model list, pricing | `jcode-provider-core` |
| Replay decision (`reasoning_replay_kind`) | the dispatching runtime; `MultiProvider` delegates |
| Tool-set lifetime | `jcode_app_core::tool::tool_set` |
| Suppression of invalid thinking | `jcode_app_core::context::reasoning_invalidation` |
| Delivery of dynamic context | `Session::append_context_delivery` and its callers |
