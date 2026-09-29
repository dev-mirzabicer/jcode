# Claude OAuth provider parity: acceptance ledger

This ledger records the evidence for the INT-01 intervention, which makes a
Claude model driven over Claude OAuth see the same system prompt, tools,
transcript and dynamic context as a GPT model over OpenAI OAuth. Each work
package appends its section. Requirement identifiers (R01–R16) and decisions
(D1–D13) come from the downstream program dossier
(`jcode_program/interventions/INT-01-claude-oauth-parity/`).

## Current behavior after WP-01

- **One tool surface.** Claude receives every registry tool under its registry
  name, with its registry description and the Anthropic dialect of its schema
  (`jcode_provider_anthropic::anthropic_input_schema`). OAuth and API-key
  requests carry identical tools. The hand-written Claude Code stand-ins
  (`Agent`, `Bash`, `Read`, `Edit`, `Write`, `Glob`, `Grep`, `Skill`) and the
  OAuth name map are gone.
- **Name policy.** Any future provider rename must be an entry in
  `jcode_provider_core::tool_name_policy` citing its evidence. The Anthropic
  table is empty. `ProviderToolNamePolicy::validate` enforces evidence,
  bijection, registration and collision rules, and the parity test runs it
  over the real registry.
- **Legacy names.** For one release after v0.75, a tool call named with a
  former Claude Code name (`Bash`, `Agent`, `ScheduleWakeup`, ...) on the
  OAuth route still resolves to the registry tool
  (`anthropic_decode_legacy_oauth_tool_name`, plus the matching aliases in
  `jcode_tool_types::resolve_tool_name` for `batch` subcalls). Remove both
  together after that release.
- **Parallel calls.** Every Anthropic request with tools sends
  `tool_choice: {"type": "auto", "disable_parallel_tool_use": true}`,
  matching GPT's `parallel_tool_calls: false` (D1). jcode's `batch` tool is the
  parallelism mechanism. The value is identical on every request so it never
  invalidates the cached message prefix.
- **Strict schemas.** Not sent to Claude. The shared strict normalizer's
  output is rejected by the Anthropic API (see the contract below), so
  `strict` stays unset on Claude (R04 recorded rejection).
- **Decoder contract.** Every `Tool` implements `decode_input`, which decodes
  input exactly as `execute` does without executing. Field-combination rules
  that the flat advertised schema describes in prose (an action's own argument,
  subagent creation-only settings) remain `execute`'s responsibility.
- **Migration.** Existing Claude sessions store registry tool names, so no
  persisted data changes. Their next request replays `bash` where the old
  request sent `Bash`: a one-time, accepted cache transition.

## Live provider contract

`jcode provider-doctor claude --contract claude-oauth --model <id>` runs the
contract probe (`crates/jcode-provider-doctor/src/claude_contract.rs`). It uses
the runtime's production request builder, token resolution and OAuth
attribution headers, sends about seventeen requests (spending subscription
quota), writes a redacted JSON report (signatures and redacted-thinking data
replaced by their length, no credential material) to
`~/.jcode/provider-contract/` or `--contract-out`, and exits non-zero when an
outcome drifts from the recorded Gate 0 observation. Probes a model did not
produce the needed turn for are listed as report notes.

With `--capture-sse <dir>` it instead records three raw streaming responses
(the thinking task with `display: "summarized"`, the same with the default
`"omitted"`, and the next turn replaying the first under `error`) as test
fixtures; see `crates/jcode-provider-anthropic-runtime/fixtures/sse/`.

| Probe | Question |
|---|---|
| `surface` | The production request (every built-in tool, full schemas, parallel calls disabled) is accepted |
| `strict_raw` | `strict: true` on the bash tool's Anthropic schema without strict normalization is rejected |
| `strict_normalized` | `strict: true` with the shared strict normalizer on exactly the tools GPT marks strict (decides R04) |
| `binding_drop_block` | `thinking.block_binding` with beta `thinking-binding-controls-2026-08-01` is accepted |
| `temperature_without_thinking` | `temperature: 1.0` with thinking omitted is tolerated |
| `T1` | Signed thinking is produced before a `tool_use` |
| `T2` | Unchanged replay is accepted |
| `T3` | An edited earlier message without a binding field is silently accepted |
| `T3b` | The same edit with `prefix_mismatch_behavior: "error"` is rejected on prefix-bound models |
| `T3c` | The same edit with `drop_block` reports `prefix_binding_mismatch` on prefix-bound models |
| `T7` | A per-turn change appended to `system` invalidates earlier thinking on prefix-bound models |
| `T4` | A doubled (spliced) signature is not rejected |
| `T5` | Thinking moved after `tool_use` is not rejected |
| `T6` | Stripping every thinking block is accepted |
| `T8a` | A second signed-thinking turn over an unchanged history is accepted |
| `T8` | Stripping the first turn's thinking (a leading run) keeps the second turn's thinking valid under `error` |

Which probes expect prefix-binding outcomes follows the runtime's per-model
`reasoning_binding` capability (D11). Production requests for prefix-bound
models carry `thinking.block_binding`; the probes that ask what happens
without it (`T3`, `T4`, `T5`, `T6`) remove it, and the binding beta header is
sent exactly when a request body carries the control.

## Gate 0 record (design session, 2026-09-29)

Run by the design session's out-of-tree harness at `ddd69a4a6` plus the D13
client-version sync (`7b29765bd`); route native Anthropic runtime over OAuth;
effort `low` (probes) or `high` (T1); `display: "summarized"`.

| Probe | Opus 5.5 | Opus 5 | Sonnet 5.5 | Sonnet 5 |
|---|---|---|---|---|
| Stub payload control | 200 | 200 | 200 | 200 |
| G0.1 real lowercase names, full schemas | 200 | 200 | 200 | 200 |
| G0.6 `disable_parallel_tool_use` | 200 | 200 | 200 | 200 |
| G0.7 `strict` on raw bash schema | 400 | 400 | 400 | 400 |
| G0.2 `block_binding` + beta | 200 | 200 | 200 | 200 |
| G0.5 `temperature: 1.0`, no thinking | 200 | 200 | 200 | 200 |
| T3 edited history, no field | 200 | 200 | 200 | 200 |
| T3b edited history, `error` | 400 | 200 | 400 | 200 |
| T3c edited history, `drop_block` | dropped | `[]` | dropped | `[]` |
| T7 reminder appended to `system` | dropped | `[]` | dropped | `[]` |
| T2, T4, T5, T6 | 200 | 200 | 200 | 200 |

`claude-fable-5` returned 429 `credits_required` on this subscription.

## WP-01 in-repository reproduction (2026-09-29)

Route Claude OAuth, native Anthropic runtime; effort `low` for single-turn
probes and `high` for the thinking chain; `display: "summarized"`; candidate
binary built from `mirza/int01-wp01-tool-surface`. The complete built-in
surface was 39 tools (the ordinary session set plus `selfdev`,
`debug_socket`, the four ambient tools and `mcp`).

| Model | Run (UTC) | Result |
|---|---|---|
| `claude-opus-5-5` | 2026-09-29T11:13:10Z | Every Gate 0 probe agrees; no drift |
| `claude-sonnet-5-5` | 2026-09-29T11:13:54Z | Every Gate 0 probe agrees; no drift |
| `claude-opus-5` | 2026-09-29T11:14:21Z | Every Gate 0 probe agrees; no drift |

Findings:

- **R01/R02 live.** The full 39-tool production surface with parallel calls
  disabled was accepted by all three models (`surface`, HTTP 200).
- **R03 live.** `tool_choice.disable_parallel_tool_use` accepted on all three
  (G0.6 reproduced inside `surface` and every later probe).
- **R04 rejected.** `strict_normalized` returned 400 on all three models:
  `Invalid schema: Enum value 'grep' does not match declared type '['string', 'null']'`.
  The shared normalizer makes an optional enum property nullable through a
  type array without adding `null` to the enum, which OpenAI accepts and
  Anthropic rejects. The API reports only the first violation. Per the design
  rule, `strict` stays unset on Claude; enabling it would need an
  Anthropic-specific strict dialect, which would also make Claude emit every
  optional field as `null`.
- **Binding.** Opus 5.5 and Sonnet 5.5 detect a changed prefix (`T3b` 400,
  `T3c`/`T7` `thinking_dropped` / `prefix_binding_mismatch` at
  `messages.1.content.0`); Opus 5 does not. The account is still not enforced
  by default (`T3` 200).
- **Stream order varies.** `T1` returned `thinking, text, tool_use` on Opus
  5.5 and `thinking, tool_use` on Sonnet 5.5 and Opus 5 in this run; Gate 0
  saw text between thinking and tool use on Sonnet 5.5. Ordered capture (WP-02)
  must handle both.

The redacted reports are retained in the program evidence directory
(`evidence/wp01-contract-2026-09-29/`).

## WP-01 activation and live session smoke

Activated build `cabe97cc4-dirty-d02092be954f` (v0.75.469-dev) through a
coordinated `selfdev build-reload`. The current and shared-server channels
both point at it, and the shared server reports `cabe97cc4`.

On 2026-09-29 an owned headless fixture session on Claude OAuth,
`claude-opus-5-5` (default effort), in a scratch Git directory, received one
prompt asking for seven tool calls in sequence. The persisted transcript
shows each call under its registry name, one tool call per assistant message
(parallel calls disabled), and every result without an error flag:

| Tool | Evidence |
|---|---|
| `read` | File content returned |
| `edit` | `beta: 2` replaced by `beta: 20`; the file on disk changed |
| `bash` | `cat` output of both files |
| `batch` | Two `read` subcalls, `2 succeeded, 0 failed` |
| `todo` | One completed item recorded |
| `get_catalog` | Profiles and model aliases listed |
| `subagent` | `agent: global:jcode`, `model_alias: fast-worker`, `permission: read_only`; child session created (the roster routed it to `gpt-5.6-terra`) and replied `ready` |

The session also carried external MCP tools (`mcp__node_repl__*`), which
Claude received under their registry names.

## WP-01 acceptance

Accepted by Mirza on 2026-09-29 and published to `main` at `0de0a4ebf`
(2026-09-29T11:57:31Z). Activated runtime `cabe97cc4-dirty-d02092be954f`, with
running, current and shared-server channels equal and the canary passed. The
activation was requested from a debug-created selfdev session whose connection
ended with the reload, so its pending activation was completed through
`jcode_build_support::complete_pending_activation_for_session` on the evidence
above.

Maintainers driving selfdev through `jcode debug`: a debug-created selfdev
session is a live agent that jcode wakes when its build completes. Destroy it
right after queueing a build.

## WP-01 deterministic evidence

- `jcode-app-core` `tool::tests::provider_parity`:
  - `claude_and_gpt_receive_the_same_tool_surface`: names, descriptions and
    schema provenance through both production builders, the name-policy
    rules and the Anthropic name pattern, over the complete built-in surface.
  - `dialect_normalizers_preserve_every_tool_schema`: the Anthropic dialect and
    the OpenAI dialect plus strict normalization keep every property,
    `required` entry, enum value, type and description, with no top-level
    combinator widenings recorded today.
  - `every_advertised_minimal_input_decodes_through_the_real_tool`: the
    minimal input of each registry schema and each Anthropic wire schema
    decodes through `Registry::decode_input` and the tool's `decode_input`.
  - `a_drifted_stand_in_schema_is_rejected`, `minimal_instances_follow_the_schema`:
    the checks are not vacuous.
- Baseline failure: the same checks pointed at the payload the production code
  produced at `ddd69a4a6` (captured in the program's
  `evidence/wire-2026-09-29/`) fail. Parity reports 99 violations (six stubs
  first, remapped names, stand-in descriptions and schemas). The decoder check
  rejects exactly the `Agent` stand-in: ``unknown field `description` ``.
- `jcode-provider-anthropic` `tool_surface_tests`, `jcode-provider-core`
  `tool_name_policy` and legacy-decoder tests, `jcode-provider-doctor`
  `claude_contract` tests.

## Current behavior after WP-02

- **Per-block capture.** The Anthropic runtime accumulates each thinking block
  on its own and emits one finalized `StreamEvent::ReplayableReasoning` when
  the block ends. Signed blocks with empty text (the default
  `display: "omitted"`, and progress updates) and `redacted_thinking` blocks
  are kept. `ThinkingDelta` drives live display only.
- **One assembler.** `jcode_base::message::AssistantTurnAssembler` builds every
  stored assistant turn, in the order the provider produced it, for both agent
  turn loops, the TUI local loop and both partial checkpoints. Replayable
  blocks of the dispatching runtime's kind keep their positions. Other
  reasoning becomes a history-only `ReasoningTrace`, or generic `Reasoning`
  for runtimes that replay reasoning text.
- **Storage.** `ContentBlock::AnthropicThinking` carries the text and signature
  byte-exact plus a `binding`: the producing model, the digest of the provider
  prefix it was produced under, and the fingerprint of the thinking block
  before it. `ContentBlock::AnthropicRedactedThinking` is new. The binding is
  jcode metadata and is never sent.
- **Replay decision.** `Provider::reasoning_replay_kind` replaces the
  provider-name check. The Anthropic runtime reports `AnthropicThinking`,
  OpenAI reports `OpenAiReasoning` and OpenRouter reports `GenericReasoning`;
  the Claude CLI and other runtimes report none. `MultiProvider` delegates to
  the runtime a request dispatches to. Top-level Claude sessions keep thinking
  as traces until WP-05 activates their replay (an internal switch, removed at
  the INT-01 closeout). Children resolved through the model roster use the
  concrete runtime and replay now.
- **Binding digest** (`jcode_provider_anthropic::binding`). Following
  Anthropic's preserved-thinking rules, the digest covers the top-level
  `system`, the tools as a name-sorted set and every earlier message. Thinking
  blocks and `cache_control` markers are excluded, and objects are serialized
  with sorted keys. A replayed block is valid when the digest before its
  assistant message matches and either it is the first thinking block
  replayed or the block replayed before it is its recorded predecessor. Only
  a leading run of thinking may be removed. The first invalid block
  invalidates every later one. The runtime checks every request and logs an
  invalid replay on a prefix-bound model as an INV-1 defect. WP-03 and WP-04
  build on this API.
- **Binding policy as data** (D11). `AnthropicReasoningCaps::reasoning_binding`
  is `PrefixBound` for Opus 5.5 and Sonnet 5.5 (measured), Fable 5.1
  (documented) and unknown future generations. It is `Unbound` for Opus 5,
  Sonnet 5 (measured), Fable 5, Mythos 5 and 5.1 (documented) and earlier
  models.
- **Safety net.** Requests for prefix-bound models carry
  `thinking.block_binding` with the `thinking-binding-controls-2026-08-01`
  beta, adding `{type: adaptive}` when a request had no `thinking` (such
  models always think; no temperature is then sent). The behavior is
  `drop_block` in every runtime build and `error` under unit tests or when the
  process has `JCODE_ANTHROPIC_PREFIX_MISMATCH=error` (Mirza's decision,
  2026-09-29). `input_transformations` entries are parsed generically, logged,
  and counted in `jcode_provider_core::anthropic_binding_diagnostics`. A
  prefix mismatch is logged as an INV-1 defect and shown as a status notice.
- **Stored before bindings.** Anthropic thinking stored before WP-02 decodes
  unchanged but is never replayed (it may be spliced or reordered). Context
  control treats it as history-only. A conversation holding such blocks shows
  a one-time status notice. None existed in local session storage at WP-02.
- **OpenAI.** New GPT turns are stored in arrival order, which is the Responses
  API's output order: reasoning items before the message and function call
  (previously the message text came first). Stored older turns are
  unchanged. Continuation is count-based and unaffected.

## WP-02 live evidence (2026-09-29)

Route Claude OAuth, native Anthropic runtime; candidate debug build of
`mirza/int01-wp02-reasoning-capture`.

**SSE fixtures** (`claude-opus-5-5`, effort `high`, 16:36Z): three recorded
streams. The next-turn capture replays the first turn, rebuilt independently
from its stream, under `prefix_mismatch_behavior: "error"`. The provider
accepted it. Every response carried `input_transformations: []`, so the
production request's binding control and beta were accepted. Each response
held one thinking block (signature in one `signature_delta` after an empty
start). The omitted-display turn's block had empty text. The runtime tests
replay these streams through the production path (parser, assembler,
persistence, formatter) and require the wire blocks to equal the independent
reconstruction byte for byte.

**Contract probe** (effort `low` for single-turn probes, `high` for the
thinking chain):

| Model | Run (UTC) | Result |
|---|---|---|
| `claude-opus-5-5` | 2026-09-29T16:39:12Z | All 17 probes agree; `T3b` 400 "bound to a different conversation … first at `messages.0.content.0`"; `T3c`/`T7` `thinking_dropped`; `T8` accepted |
| `claude-sonnet-5-5` | 2026-09-29T16:40:34Z | All 17 probes agree; same binding outcomes; `T8` accepted |
| `claude-opus-5` | 2026-09-29T16:41:13Z | All 17 probes agree; no binding check (`T3b`/`T3c`/`T7` accepted); `T8` accepted |

`T8` confirms live the permissive half of jcode's validity rule: after the
first turn's thinking is stripped, the second turn's thinking stays valid under
`error`.

**Child execution path** (`claude-sonnet-5-5`, effort `high`,
`JCODE_ANTHROPIC_PREFIX_MISMATCH=error`, 16:50Z): the ignored test
`claude_child_route_replays_signed_thinking_under_error`
(`src/cli/reasoning_live_tests.rs`) resolves `claude-oauth:claude-sonnet-5-5`
through the production model roster, as a delegated child's provider is
resolved, and runs a four-step file task through the streaming turn loop that
children use. Five requests were all accepted. Thinking came from two
requests and every later request replayed it. Each stored block's predecessor
is the block before it, and no local or provider binding event was recorded
(session `session_kangaroo_1790700620006_46d218da58742b47`). A real delegated
child was not used because no roster alias routes to Sonnet 5.5 and the
`subagent` tool takes aliases only. An earlier attempt used `bash`, which has
no native command worker outside the server. Its one thinking block was also
replayed under `error` and accepted.

## WP-02 activation and live smoke

Activated build `ed379c006-dirty-10c2f7c6f4fe` (v0.75.475-dev) through a
coordinated `selfdev build-reload` at 2026-09-29T18:41Z. The build pipeline's
pre-publication smoke started the candidate server before the reload. The
running server, current and shared-server channels all report `ed379c006`. As
in WP-01, the build was requested from a debug-created selfdev session, which
was destroyed, and the pending activation was completed with
`jcode_build_support::complete_pending_activation_for_session` on this
evidence. The canary is `passed`.

On the activated server, an owned headless session on Claude OAuth
`claude-opus-5-5` (default effort) ran a read, write, read task. It made four
requests, each served by `claude-opus-5-5`, carrying the production binding
control. All tool results succeeded (`b.txt` holds `ALPHA`), and the server
logged no INV-1 event or input transformation.

## WP-02 deterministic evidence

- `jcode-provider-anthropic` `binding::tests`: digest determinism under the
  scheme label, cache markers excluded, tools bound as a name-sorted set,
  canonical key order, nested `cache_control` content still bound, thinking
  excluded from the prefix, and validity for append-only history, an edited
  earlier message, a changed system prompt or tool set, a change after a
  block, a stripped leading run, a block removed from the middle and redacted
  chaining. `context_validation_tests`: empty-text and redacted blocks
  validate and replay in stored order; unbound blocks are never replayed.
- `jcode-provider-anthropic-runtime`: per-block SSE capture and chaining, the
  recorded-stream byte-exact round trips, a recorded second turn chaining to
  the first, a synthetic multi-block stream (signed, text, empty signed,
  redacted, tool use), `input_transformations` surfacing, the binding control
  for prefix-bound and unbound models with both behaviors, the beta header,
  and the environment override.
- `jcode-provider-core`: the `reasoning_binding` entries and their agreement
  with the capability, and the diagnostics counter.
- `jcode-base`: `message::assistant_turn` (order, single storage, traces,
  OpenAI and generic kinds, whitespace, tool calls, recovery, reset),
  persistence round trips including legacy decoding,
  `provider::tests::reasoning_replay_kind_follows_the_runtime_a_request_dispatches_to`
  (Claude-direct, Claude CLI and OpenAI), and rendering of stored signed
  thinking.
- `jcode-app-core`
  `agent::tests::both_agent_loops_store_reasoning_in_stream_order_through_the_assembler`
  (blocking and streaming loops, replay on and off, with a mid-stream
  rollback). `jcode-tui` `sdk_results` checks the local partial checkpoint.
