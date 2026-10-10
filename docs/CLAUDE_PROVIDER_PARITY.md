# Claude provider parity

When jcode drives a Claude model over Claude OAuth or the Anthropic API key, the
model sees the same system prompt, tools, transcript and dynamic context as a
GPT model over OpenAI OAuth. Its signed thinking is replayed, and the prompt
cache follows the conversation. This page describes that behavior and who owns
each part. The evidence and its history are in the
[acceptance ledger](dev/CLAUDE_OAUTH_PARITY_ACCEPTANCE.md).

Three differences are intended:

- Claude OAuth requests start `system` with two identity blocks (the billing
  header and "You are a Claude agent, built on Anthropic's Claude Agent SDK."),
  as the official Agent SDK does with a custom system prompt.
- Each provider uses its own dialect for the same meaning: the tool-schema
  dialect, `is_error` instead of `[Error]` text, signed `thinking` blocks
  instead of encrypted reasoning items.
- Harness notices and tool-set changes use each provider's native operator
  channel where the model has one ([operator notices](#operator-notices),
  [tool set](#tool-set)). The stored transcript is the same for every provider;
  only the rendering differs.

The routes covered are native Anthropic over OAuth and over an API key. The
deprecated Claude CLI subprocess route does not have these properties; see
[Claude CLI route](#claude-cli-route). Bedrock, Vertex, OpenRouter and Copilot
Claude routes are not covered.

## What a Claude request contains

In render order:

| Part | Content |
|---|---|
| `tools` | The session's [tool set](#tool-set): every registry tool it was first offered, under its registry name, with its registry description and the Anthropic dialect of its schema. There are no provider-authored stand-ins. `tool_choice` is `{"type": "auto", "disable_parallel_tool_use": true}`, matching GPT's `parallel_tool_calls: false`; jcode's `batch` tool is the parallelism mechanism. `strict` is not sent (the Anthropic API rejects the shared strict schemas). |
| `system` | On OAuth, the two identity blocks, then the static system prompt: the composed instructions, plus the active skill and the dormant Swarm effort directive when present. Nothing changes per request. |
| `messages` | The projected transcript (context control applies), with signed thinking replayed exactly as stored. Per-turn system reminders, the batch nudge, reload continuations and tool-set notices are ordinary persisted transcript content ([delivery contract](NOTIFICATIONS.md#delivery-of-model-visible-context)). |

Between recorded transitions each request is the previous one with content
appended (append-only history). This holds across a reload, a server restart
and a resume, because everything a request is built from is persisted with the
session: the transcript, the context view, the tool set, and, for sessions
whose system prompt is frozen (every interactive session), the system prompt.
Programmatic sessions without a frozen prompt compose it from the current
instruction sources on each request, so an instruction edit changes their
prefix.

The recorded transitions are: a context-control apply, revert or reapply;
clear, rewind or a provider or model switch (including a runtime model
fallback); a static-prompt transition (skill activation, Swarm effort
directive, instruction reload or agent replacement); a tool-set change that a
provider's `tools` array must carry (below); and a client-identity sync
(below). Each is journaled (`CACHE_INVALIDATION_DOCUMENTED`) and named as the
cause of any thinking it invalidates.

## Operator notices

Per-turn system reminders, the batch nudge and tool-set notices are harness
guidance, not something the person typed. Each such delivery stores the
authority it asks for (`operator`) with its origin. On the request path a
runtime that has an operator channel receives it as an operator notice and
renders it natively:

- **Anthropic.** A `role: "system"` message inside `messages`, carrying the
  notice's body without the `<system-reminder>` wrapper, on models that accept
  one: Opus 5.5, Sonnet 5.5, Opus 5 (measured), Opus 4.8, Fable 5 and 5.1,
  Mythos 5 and 5.1 (documented). The API requires such a message to follow a
  user message and to be the last message or be followed by an assistant
  message. Where that placement does not hold (a reminder-only resume turn
  after an assistant message, for example), and on every other model, the
  notice is the stored user-role text, exactly as before. Adjacent notices
  are adjacent system messages.
- **OpenAI Responses.** A `developer` message in place. The ChatGPT OAuth
  backend accepts it and keeps its prompt cache (measured 2026-10-02 on
  GPT-5.6 Sol). `JCODE_OPENAI_OPERATOR_MESSAGES=0` falls back to the stored
  user-role text for a process.
- **Every other runtime.** The stored user-role `<system-reminder>` text.

The rendering is a pure function of the stored history and the model's
capability entry (`anthropic_conversation_caps` in `jcode-provider-core`), so
it is the same on every request. The one place it can change is a notice that
never got a reply (its request failed) and is then followed by a new user
message: it becomes user text. No thinking was produced after it, so nothing
is invalidated. Deliveries stored before operator authority existed keep the
user form. Sonnet 5 accepted a system message in the probe, but Anthropic
documents it as unsupported, so jcode keeps the user form there.

## Tool set

Tools render first in every provider prefix, and Claude binds each thinking
block to the tool set. So the set a session's first request advertised is
frozen and persisted with the session (`Session::tool_set`). A reload, a
restart or a resume sends the same tool bytes.

Before every request the frozen set is compared with the live registry. A
tool added, removed or redefined (an MCP server connecting or going away, the
`mcp` tool, a new binary whose tool text differs) is recorded and announced
once, as an appended [tool-set notice](NOTIFICATIONS.md#tool-set-notices).
What the provider's `tools` array then holds depends on the provider:

| Change | Anthropic models that take tool changes inside a message (Opus 5.5, Sonnet 5.5, Opus 5, Opus 4.8, Fable 5/5.1, Mythos 5/5.1) | Every other model and provider |
|---|---|---|
| Tool added | `tool_addition` by value (beta `inline-tools-2026-09-15`) in the notice's system message; `tools` unchanged | The definition is appended to `tools`: a recorded `tool set change` transition |
| Description changed | `tool_addition` by value replaces it; `tools` unchanged | `tools` keeps the first-sent bytes; the notice carries the new description |
| Input schema changed | `tool_addition` by value replaces it; `tools` unchanged | The definition in `tools` is replaced: a recorded `tool set change` transition |
| Tool removed | `tool_removal` by name in the notice's system message; `tools` unchanged | The definition stays in `tools` |

On the left the cache and every earlier thinking block survive the change. A
`tool set change` transition rebuilds the cache and, on a model that binds
thinking, suppresses the earlier thinking explicitly, with that cause.

Further rules:

- A call to a removed tool returns "no longer available in this session".
- MCP servers connect in the background. A frozen MCP tool that is missing
  while its servers are still connecting after a start is kept, and a call to
  it returns "its MCP server is reconnecting". If the server settles without
  it, it is removed like any other tool.
- A globally unavailable tool (Swarm while it is disabled, or a retired
  feature such as `initiative` while its gate is off) is never offered, even
  when the frozen set held it. Its removal is recorded as withdrawn, in the
  persisted record, so every array stays a function of the record alone and
  each change of availability is compared and recorded. On the left of the
  table it is removed like any other tool: `tools` keeps its first-sent bytes
  and the notice carries the `tool_removal` of that declared tool (no cache
  or thinking loss). Elsewhere its definition leaves `tools` at a recorded
  `tool set change` transition. If it becomes available again, it returns as
  an addition: by value inside the notice on the left, at another recorded
  transition elsewhere.
- A rendered tool change names only a tool the request knows: one in its
  `tools`, or added by value earlier in its messages. The API rejects any
  other reference ("tool_addition/tool_removal references unknown tool"), so
  such a change keeps only its notice text.
- A notice that becomes user text (see [operator notices](#operator-notices))
  moves its tool changes to the next valid position: the next notice kept as
  a system message, ahead of that notice's own changes, otherwise a system
  message before the next reply. The model is always told the changes in the
  order they happened.
- In-message changes exist only while their notice is in the projected
  history. When a context summary or a rewind hides a notice, the next
  request announces its changes again, appended, so the model's tools never
  regress. If both the original and the repeat are later visible, the repeat
  is not rendered twice.
- After a switch to another provider or model, the array is whatever that
  runtime's rule gives for the same record. Switches are already transitions.
- Clear and a new session start without a record and freeze a new set at
  their first request. A session stored before tool sets were persisted
  freezes the registry of its next request.
- A tool whose name the runtime's API would reject is never advertised there
  (Anthropic: 1 to 128 characters of letters, digits, `_` and `-`; OpenAI: 1
  to 64). One such name would fail every request. The person gets a status
  notice naming the tools; a provider rename, if one is ever needed, is a
  declared entry in `jcode_provider_core::tool_name_policy`.

The agent loops and the TUI local loop share the planner
(`jcode_app_core::tool::tool_set`).

## Thinking

- **Capture.** Each `thinking` and `redacted_thinking` block is stored per
  block, in stream order, byte-exact, including empty-text signed blocks, with
  the producing model and a binding: a digest of the prefix it was produced
  under and its predecessor block.
- **Lossless stream.** The response body is framed as bytes: each line is
  decoded as strict UTF-8 only once complete, so a character split across
  network chunks is never damaged, and LF, CR and CRLF line endings frame the
  same events. A turn is complete only when the provider says so. A body
  that ends without `message_stop`, with a content block still open, with
  leftover data or with a malformed known event is an incomplete response:
  a transport fault that is retried before any output and never stored as a
  completed turn. Cancellation stays a separate outcome.
- **Replay.** Top-level Claude sessions and roster-routed children replay stored
  thinking unchanged and in place. Blocks stored before bindings were recorded
  (before INT-01/WP-02) are never replayed.
- **Binding.** Opus 5.5, Sonnet 5.5 and Fable 5.1 bind thinking to the
  conversation before it: `system`, the tools as a name-keyed set, and every
  earlier message. When a change leaves a replayed block bound to a prefix the
  next request no longer has, jcode suppresses it explicitly, persisted and
  visible, before sending ([context control](CONTEXT_CONTROL.md#replayed-reasoning-bound-to-its-prefix)).
  Which models bind is policy as data: `anthropic_reasoning_binding` in
  `jcode-provider-core`, one evidence-cited entry per model.
- **Exact planning.** Which blocks to suppress is decided on the request
  exactly as the formatter builds it, including a message that becomes empty
  and is dropped once its only block is suppressed. The request is validated
  once more just before it is sent, and jcode never sends a block its own
  analysis finds invalid: the request goes back to be planned again instead.
- **Best-effort retention.** jcode keeps every block the provider can still
  use. A suppressed block comes back only when its recorded binding proves it
  valid again (a reverted summary, for example); the blocks produced while it
  was absent are then suppressed. A switch to a model or route that does not
  bind changes nothing: nothing is lifted and nothing is stripped, so
  switching back finds the same set. jcode sends the same transcript,
  thinking included, to every Claude model and lets the API leave out blocks
  the current model cannot read (`model_binding_mismatch`); those drops lose
  nothing on the way back and are not recorded as suppressions.
- **Provider feedback.** A drop the API reports for a changed prefix
  (`input_transformations`, any type or reason jcode does not know to be a
  routing drop) and a rejection that names a block under `error` are mapped
  back to the stored blocks. That block and every later replayable block
  (the API drops the whole run) join the managed set with the cause
  "provider reported", and stay suppressed until the session is cleared. The
  request rejected under `error` is planned again once. The documented
  "thinking blocks cannot be modified" rejection is handled the same way.
- **Safety net.** Requests to binding models carry
  `thinking.block_binding.prefix_mismatch_behavior: "drop_block"` (beta
  `thinking-binding-controls-2026-08-01`). Setting
  `JCODE_ANTHROPIC_PREFIX_MISMATCH=error` makes a mismatch fail the request
  instead; verification runs use it.
- **Display.** Every request that carries a thinking configuration asks for
  `display: "summarized"`, including requests where jcode adds the
  configuration only to carry the binding control. It returns reasoning
  summaries and the progress notes the model writes between tool calls.
  `"updates"` would return only the progress notes, so it is not used.

## One request plan

The credential route, the model and every model-dependent parameter (thinking
configuration, effort, `max_tokens`, sampling, binding control, beta headers)
are resolved once per request, before reasoning is reconciled, and carried
unchanged through validation, formatting and sending.

- Changing the credential mode forgets the previous route, so the next
  request is planned for the route it will actually use.
- When the selected model is not found or out of quota and the runtime moves
  to another model, the request is not patched: the switch is persisted,
  recorded as a `provider model fallback` transition, and the agent plans the
  request again for the new model. Callers that cannot plan again get every
  parameter rebuilt for the new model.
- The retry for "reasoning parameters unsupported" keeps the binding control
  on binding models.

## Reasoning effort and sampling

A session stores what it asked for, not a resolved value: `Default` or
`Explicit(level)` (`Session::reasoning_effort_intent`). The effective effort
is resolved for the current model on every request.

- `Default` follows the runtime's default for whichever model is current: the
  configured default (`provider.anthropic_reasoning_effort`), else the model's
  entry below. `/effort default` selects it.
- `Explicit(level)` is applied again after every model switch. A model that
  does not offer the level runs at its default while it is current; the
  intent keeps the level.
- Restore, resume, a model switch and a runtime model fallback all apply the
  intent. Remote clients receive the effective value in the model-changed and
  effort-changed events.
- Sessions stored before intents existed kept the resolved default as a
  string, as if it had been chosen. A stored value equal to the runtime
  default for the session's model, or to the default that model had before
  (`medium` on Opus 5.5, whose default is now `high`), was almost certainly
  that default and becomes `Default`; such a session follows today's
  default. Any other stored value becomes `Explicit` and is kept.

Per-model data in `jcode-provider-core` (`anthropic_reasoning_caps`):

| Model | Default effort when none is configured | Effort `none` |
|---|---|---|
| Opus 5.5 | `high` (jcode's choice; the API default is `medium`) | not offered; means `low` (thinking cannot be disabled) |
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
3. the last block of the user or operator message before the latest assistant
   response, where the previous request wrote, so each request reads exactly
   the previous entry;
4. when more than 15 lookback positions separate 3 and 2, one more marker 15
   positions before the newest (the API looks back 20 positions; a run of tool
   calls or of tool results is one position).

Thinking blocks never carry a marker. Every marker uses the configured TTL,
one hour by default (`set_cache_ttl_1h`). A person's pause between turns often
exceeds five minutes, which would expire a five-minute entry and write the
whole conversation again; the one-hour write premium applies only to the
content each request appends. The measured cadence behind this choice is in
the ledger.

## Client identity

The Claude OAuth billing header carries the client version jcode reports. A
session records the identity text its requests carried, and a change between
two requests (a version sync shipped in a new binary) is journaled as an
`OAuth client identity sync` transition. Measured on 2026-10-01 (probe G6.6),
the API neither caches nor binds that block, and the binding digest treats it
as fixed text, so a sync currently invalidates nothing.

## Models

`claude-opus-5-5` is the default Claude model, at default effort `high`. Opus
5.5, Opus 5, Sonnet 5.5 and Fable 5.1 are in the curated model list
(`ALL_CLAUDE_MODELS`) and API pricing, with their 1M context window and 128K
output.

## Adding or updating a Claude model

Every model-specific behavior on this page is data in `jcode-provider-core`.
Adding a model, or following a change Anthropic makes to one, is a set of
entries with their evidence, not new mechanism.

1. **List and price it.** `ALL_CLAUDE_MODELS` (`models.rs`; position 0 is the
   default), the API pricing table (`pricing.rs`), the context window and the
   maximum output (`anthropic_max_output_tokens` and the context mode in
   `anthropic.rs`).
2. **Set its request parameters** in `anthropic.rs`, each with a comment
   citing documentation or a measurement:

   | Entry | Question |
   |---|---|
   | `anthropic_effort_caps` / `anthropic_selectable_efforts` | Which effort levels does it offer? This one ladder serves the runtime and remote clients. |
   | `anthropic_default_reasoning_effort` | Which effort does jcode use when none is configured? |
   | `anthropic_thinking_off` | How is effort `none` expressed: omitted thinking, `{type: "disabled"}`, or not possible (then `low`)? |
   | `anthropic_accepts_sampling_parameters` | Does it still accept `temperature`? |
   | `anthropic_reasoning_binding` | Does it bind thinking to its prefix (`PrefixBound`) or not (`Unbound`)? |
   | `anthropic_conversation_caps` | Does it accept mid-conversation `role: "system"` messages and in-message tool changes? |

   An unknown future generation defaults to `PrefixBound` (over-suppression
   loses some reasoning but never produces a rejection) and to no
   mid-conversation features (the user-role form always works).
3. **Measure, do not assume.** With a credential that reaches the model:

   ```text
   jcode provider-doctor claude --contract claude-oauth --model <id>
   jcode provider-doctor claude --contract claude-oauth-boundaries --model <id>
   ```

   The first reports whether the model checks bindings: `T3b` is rejected and
   `T3c`/`T7` report `prefix_binding_mismatch` on a binding model, and all
   three are accepted without a report on one that does not bind. The second
   reports whether it accepts system messages (`G6.1`), in-message tool
   changes (`G6.2`), and how it treats thinking across a switch (`G6.5`).
   Both spend quota and write redacted reports. Where documentation and a
   measurement disagree, the measurement describes current behavior; record
   the disagreement beside the entry and in the ledger.
4. **Run the tables' tests** (`cargo test -p jcode-provider-core`) and the
   runtime's (`cargo test -p jcode-provider-anthropic-runtime`); both assert
   each behavior for a binding and a non-binding model and for models with
   and without the mid-conversation features.

When Anthropic changes an existing model:

- **Binding relaxed or introduced.** Change the `anthropic_reasoning_binding`
  entry. Stored blocks need no migration: every block keeps its binding
  record whatever the policy.
- **A new drop reason or transformation type.** Nothing to change:
  `input_transformations` is parsed generically, and an unknown prefix-related
  drop already feeds the managed set. Add the reason to the routing-drop list
  in the runtime only if it is documented as a drop that loses nothing.
- **Mid-conversation features added or withdrawn.** Change the
  `anthropic_conversation_caps` entry. Renderings of stored notices follow
  the entry, which is a prefix change for open sessions on that model, like
  any other model-behavior change.
- **A new client version requirement** (`claude_code_version_too_old`). Sync
  the version constants as upstream does; open sessions record the sync.

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
  `--contract claude-oauth-boundaries` runs the mid-conversation and switch
  probes. Both spend subscription quota.
- `CACHE_INVALIDATION_DOCUMENTED` log lines name every recorded transition;
  `INV-1` warnings name a replayed block found invalid at request time.

## Ownership

| Concern | Owner |
|---|---|
| Tool dialect, parallel policy, cache placement, binding digests, suppression planning, operator-notice rendering | `jcode-provider-anthropic` |
| Request planning, stream decoding and completion, safety net, provider feedback, effort resolution | `jcode-provider-anthropic-runtime` |
| Per-model capabilities and policy (binding, thinking off, sampling, default effort, mid-conversation features), tool-name rules, model list, pricing | `jcode-provider-core` |
| Replay decision (`reasoning_replay_kind`) | the dispatching runtime; `MultiProvider` delegates |
| Tool set: record, comparison, notices | `jcode_session_types::tool_set`, `jcode_app_core::tool::tool_set` |
| Suppression of invalid thinking | `jcode_app_core::context::reasoning_invalidation` |
| Operator authority on the request path | `jcode_app_core::context::operator_notice` |
| Effort intent | `Session::apply_reasoning_effort_intent` and its callers |
| Delivery of dynamic context | `Session::append_context_delivery`, `Session::append_tool_set_delivery` and their callers |
