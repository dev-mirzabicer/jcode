# Claude OAuth provider parity: acceptance ledger

This ledger records the evidence for the INT-01 intervention, which makes a
Claude model driven over Claude OAuth see the same system prompt, tools,
transcript and dynamic context as a GPT model over OpenAI OAuth. Each work
package appends its section. Requirement identifiers (R01–R28) and decisions
(D1–D18) come from the downstream program dossier
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

## WP-02 acceptance

Accepted by Mirza on 2026-09-29, around 18:59Z, on candidate `ff4d975e8`.
Activated runtime `ed379c006-dirty-10c2f7c6f4fe`, with running, current and
shared-server channels equal and the canary passed; the source differs from
the runtime only by documentation commits. Top-level Claude replay remains off
until WP-05. Until WP-03 moves per-turn reminders out of `system`, a Claude
child's follow-up turn can drop its earlier thinking under `drop_block`, which
is logged as an INV-1 defect with a status notice.

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

## Current behavior after WP-03

- **One delivery path for dynamic context** (D2). Per-turn system reminders,
  the batch nudge and reload-resume continuations reach the model only as
  persisted transcript content: a user-role
  `<system-reminder>\n…\n</system-reminder>` message with `display_role:
  System` and a structural `ContextDelivery` origin (channel and a fingerprint
  of the exact text). GPT and Claude receive the same bytes. The contract is
  in [`NOTIFICATIONS.md`](../NOTIFICATIONS.md#delivery-of-model-visible-context).
- **Once per occurrence** (Mirza's decision D-WP03-1, 2026-09-30). A turn's
  reminder is committed with the input it accompanies, in the same save and
  durable input receipt. A safe-boundary group delivers its shared reminder
  once, after its last input. A reminder-only input (a reload resume) has the
  delivery as its content, so no empty prompt is stored. The batch nudge is
  persisted when it fires. Identical text on a later occurrence is delivered
  again; the design's per-channel text dedup would have dropped the second
  background-task reminder and emptied a second reload resume.
- **Static system prompt** (R08, D8). Claude's `system` is the two OAuth
  identity blocks plus the cached static prompt; GPT's `instructions` is the
  static prompt. Active-skill text and the dormant swarm effort directive are
  static sections composed by `jcode_base::prompt::compose_static_system_prompt`.
  A skill activation and a switch into or out of a swarm effort are recorded
  in the cache-invalidation journal. The per-request dynamic part, its
  provider parameter (`Provider::complete_split*`), the Anthropic dynamic
  system block and `messages_with_dynamic_system_context` are deleted.
- **Formatter** (R10). Tool results lead every merged Anthropic user message
  (stable partition, as upstream). The trailing-assistant `Continue.` repair
  remains only as a guard logged as an INV-1 defect.
- **Dormant channels** (D9). Memory keeps its trailing request-only message in
  both the agent loops and the TUI local loop
  (`PendingMemory::provider_message`); a reactivation must deliver through
  the persisted path. The swarm directive renders its managed source each
  request; a Swarm reactivation must freeze it or record source edits.
- **History.** Deliveries render as system messages showing their body, in
  server History and the TUI local view. Legacy messages recognized only by
  their `<system-reminder>` prefix stay hidden.
- **Migration.** Old sessions have no deliveries; their next reminder is
  delivered once as new transcript content. GPT sessions now keep reminders
  in history rather than receiving a fresh copy after the latest prompt on
  every request.

## WP-03 append-only evidence

`agent::context_delivery_tests::scripted_session_is_append_only_on_both_production_builders`
drives the real agent loops through 31 requests with a recording provider:
turns with and without reminders, a batch nudge after repeated single-tool
rounds, a safe-boundary background input with its own reminder, one skill
activation (the only declared transition), a failed request after a persisted
tool result, a reload into a fresh Agent, and two reminder-only resumes with
identical text. Each request is formatted exactly as the Anthropic
(`build_system_param`, `format_messages`, `format_tools`) and OpenAI
(`build_tools`, `build_responses_input`) runtimes format it, with cache
markers removed. Between consecutive requests `system` and `tools` must be
byte-identical except across the declared transition, and the (role, block)
sequence must only grow. Every occurrence must be persisted exactly once with
its structural origin, and no empty prompt may be stored.

At `9353d296d` the same harness failed on both builders: Claude's `system`
changed on every reminder turn (requests 3, 11, 12, 16, 18, 20, 23, 24, 28),
GPT's reminder message moved or disappeared (requests 3, 9, 12, 14, 16, 18,
20, 23, 24, 28), the batch nudge vanished after one request (request 9, both),
and no reminder was ever persisted. After WP-03 it passes, together with
`one_boundary_group_delivers_its_shared_reminder_once_after_its_inputs`.

## WP-03 live evidence (2026-09-30)

`cli::startup::delivery_live_tests::append_only_delivery_live` (ignored; run
with `cargo test -p jcode --lib append_only_delivery_live -- --ignored`)
runs two reminder turns, a reload and a reminder-only resume through the
streaming turn loop.

- **Claude OAuth, `claude-sonnet-5-5`, effort `high`,
  `JCODE_ANTHROPIC_PREFIX_MISMATCH=error`**, concrete Anthropic runtime (the
  path children use, which replays signed thinking), 2026-09-30T11:12Z:
  passed. Eight assistant turns, three deliveries, two signed thinking blocks,
  written values `29` and `81`, no binding events. The first turn's thinking
  was replayed on every later request, after the second turn's delivered
  reminder, after the reload and on the reminder-only resume, so the earlier
  prefix stayed byte-identical; Gate 0 T7 showed the former `system`
  placement invalidating such blocks. Session
  `session_guppy_1790766772709_86910e63c240abe5`. Two earlier attempts are
  recorded: one ended on a 429 rate limit, and one passed every request but
  produced no thinking, so the script now asks for careful computation.
- **OpenAI OAuth, `gpt-5.6-sol`, effort `low`**, 2026-09-30T11:54Z: passed.
  Eight assistant turns, three deliveries at the same positions as on Claude
  (after each prompt, and as the resume turn's whole content), three
  encrypted reasoning items replayed, written values `29` and `81`. Session
  `session_dromedary_1790769251846_0045b3301b08f740`. Two earlier attempts
  (11:13Z and 11:41Z) were blocked by the account's `usage_limit_reached`
  before any response; Mirza switched the OpenAI OAuth account.

## WP-03 acceptance

Accepted by Mirza on 2026-09-30 on candidate `3aae9687f`, after the Claude
leg; the GPT leg above completed the live evidence afterwards. Published to
local and downstream `main` at 2026-09-30T11:44:24Z. Activated runtime
`debd94e90-dirty-e56fbd1ff5b5` (running, current and shared-server channels
equal, canary passed); later commits are test-only or documentation. WP-04
consumes the recorded transitions (skill activation, swarm effort directive)
and the delivery owner as reasoning-invalidating context.

## WP-03 activation and live smoke

Activated build `debd94e90-dirty-e56fbd1ff5b5` (v0.75.480-dev) through a
coordinated `selfdev build-reload`, requested at 2026-09-30T09:33:40Z from a
debug-created selfdev session. An earlier request was correctly refused as
superseded because the worker changed the source tree during the build. The
build request reports "published and smoke-tested". The running server,
current and shared-server channels report `debd94e90`. As in WP-01 and WP-02,
the new server did not load the requesting session, so the pending
activation was completed with
`jcode_build_support::complete_pending_activation_for_session` after a
manifest backup; the canary is `passed`.

On the activated server, an owned headless session on Claude OAuth
`claude-sonnet-5-5` started a background `bash` task with `wake: true`. Its
completion arrived as a BackgroundTask input followed by a persisted
`ContextDelivery` reminder (`# System Reminder` with the managed
background-task prose), and the woken request was accepted. A tester TUI
client resuming that session rendered the delivery as a `system` message
with its body, between the background-task notice and the reply, with no
frame anomalies. The server logged no INV-1 defect, binding event or
continuation repair.

## WP-03 deterministic evidence

- `jcode-session-types` `context_delivery::tests`: one text form, fingerprint
  validation and serialization round trip.
- `jcode-provider-anthropic` `tool_results_first_tests` (R10: parallel results
  lead a message that also carries an injected input and a delivery) and the
  updated trailing-assistant guard tests. `jcode-provider-anthropic-runtime`:
  OAuth `system` is the identity blocks plus the cached static prompt, API-key
  `system` is only the static prompt.
- `jcode-base`: `context_delivery_is_persisted_and_rendered_as_a_system_message`,
  the static-prompt swarm directive and composition tests.
- `jcode-app-core`: the two harness tests above, the active-skill snapshot
  tests (skill text is a static section, frozen across disk edits, resume and
  split), and `managed_effort_is_a_static_section_and_fails_before_provider`.
- `jcode-tui`: `test_system_reminder_is_delivered_as_transcript_content_not_system_prompt`
  and the local skill and kv-cache telemetry tests.

## Current behavior after WP-04

- **One managed set** (R11, D6, Mirza's decisions D-WP04-1 and D-WP04-2,
  2026-09-30). Every replayed Claude thinking block that no longer matches its
  request prefix is held by one jcode-managed reasoning-invalidation context
  transaction (`StoredContextAuthorization::ReasoningInvalidation`), as
  `ReasoningSuppression` operations with an `Invalidated { cause }` selection,
  one per cause. `jcode_app_core::context::reasoning_invalidation` is its only
  writer. It recomputes the complete set from the WP-02 bindings in every
  apply, revert, reapply and authorized emergency transaction
  (`prepare_context_transition_for_session`) and before every request (both
  agent loops and the TUI local loop). When the set changes, the previous
  managed transaction is `Superseded` and the new one applied at the same
  revision; people cannot revert or reapply it, and undo skips it. Current
  behavior is documented in
  [`CONTEXT_CONTROL.md`](../CONTEXT_CONTROL.md#replayed-reasoning-bound-to-its-prefix).
- **Why a whole set.** DESIGN §5 staged consequential suppressions "in the same
  reviewed transaction". Revert and reapply must also suppress blocks produced
  while the reverted change was active and restore blocks that match again,
  which suppressions inside immutable user transactions cannot do. The managed
  set is recomputed and superseded instead, so every sequence of transitions
  is exact.
- **Validity.** `Provider::replayed_reasoning_invalidations(messages, tools,
  system)` reports the blocks a request with exactly that prefix would have to
  drop. The Anthropic runtime builds its production request (on the route of
  its latest request, or before one the configured credential mode) and
  applies `binding::blocks_to_suppress`: `analyze_request`'s rule with each
  invalid block already removed. Models whose `reasoning_binding` is
  `Unbound`, and every other provider, answer `None`, so nothing is staged and
  an existing set is lifted (D11).
- **Causes.** `ContextTransition { transaction_id, transition }` for blocks the
  transition itself invalidates; `RequestPrefixChanged { recorded_transitions
  }` for blocks invalid because the system prompt, tool set or credential route
  changed, with the harness transitions the agent recorded since its previous
  request (skill activation, tool-set changes, model switch, agent
  replacement). Causes survive supersession and a rewind that ends a set.
- **Review and apply.** Reviews stage exactly as apply does and show a locked
  group (invalidated by this edit, already invalid, replayed again); economics
  include the removed reasoning. A review records the request-prefix digest it
  was computed under, and apply refuses a draft whose prefix changed.
  Revert and reapply keep their confirmation; the result status reports the
  change (D-WP04-2).
- **Requests.** A request-time change is persisted before the request is sent;
  a persistence failure blocks the request. It raises a status notice and a
  `reasoning invalidation` cache-invalidation record.
- **Selections.** A person's `R` selection is recorded even for blocks jcode
  suppresses, so a later restore cannot undo it. Managed sets are left out of
  provider-kind validation, so they never block a provider switch.
- **Cost.** The request prefix is composed only when the route binds reasoning
  and the transcript holds bound reasoning, or a managed set is in force.
  OpenAI sessions and top-level Claude sessions (thinking stored as traces
  until WP-05) do no extra work.
- **Persistence.** The context-view schema version is unchanged; older state
  decodes as before. A session that holds a managed set uses new enum values
  that pre-WP-04 binaries cannot decode.

## WP-04 deterministic evidence

- `jcode-provider-anthropic` `binding::tests`: `blocks_to_suppress` keeps an
  unchanged history, suppresses the blocks after an edit and keeps the ones
  before, suppresses the blocks chained after a middle suppression, keeps the
  blocks after a leading run, suppresses everything on a changed system prompt
  or tool set, and every plan leaves only valid blocks under
  `analyze_request`. Stored and wire fingerprints agree.
- `jcode-provider-anthropic-runtime`: `replayed_reasoning_invalidations`
  through the production request builder (indices, middle chain, system and
  tool changes), across the credential route (the OAuth identity blocks are
  bound), and `None` for Opus 5 (Unbound).
- `jcode-session-types`, `jcode-context-core` validation: managed sets round
  trip; they are applied once then superseded or ended by a transcript edit;
  only one may be active; only they may carry an `Invalidated` selection, of
  signed thinking only.
- `jcode-app-core` `context::reasoning_invalidation_tests` (a provider that
  decides validity with the production Anthropic formatter and binding rule,
  every turn bound to the exact projected request): summary, distillation,
  middle suppression, keep-latest (nothing staged), revert (restores and
  suppresses), reapply, overlapping transactions in every order, Unbound
  model, switch to an Unbound model, request-time system-prompt and tool
  changes with restoration, attribution under an unreconciled prefix change,
  a person's selection of a managed block, managed revert/reapply refused and
  undo skipping it, a rewind re-staging with the original cause, a plain
  transcript never reading the prefix, review equals apply (group and
  economics), a no-op review stages nothing, apply refusing a changed prefix,
  and an authorized emergency transaction staging under its own cause with the
  reasoning in its recorded reduction.
- `jcode-app-core` `agent::reasoning_invalidation_tests`: the streaming and
  blocking turn loops suppress and persist thinking bound to a replaced system
  prompt before the next request, which then replays nothing invalid.
- `jcode-tui`: the review's locked group (wide and narrow), the apply
  confirmation line, a managed transaction in history (label, disabled Revert
  and Reapply, `r`/`p` refused with a notice), per-cause provenance detail, and
  the local request gate. Debug fixtures `reasoning-invalidation-review`,
  `-confirmation`, `-history` and `-detail`.
- `jcode-protocol`: the new result, preview and draft fields round-trip and
  default on older wire forms.

## WP-04 live evidence (2026-09-30)

`cli::startup::invalidation_live_tests::context_transitions_live` (ignored;
run with `JCODE_ANTHROPIC_PREFIX_MISMATCH=error cargo test -p jcode --lib
context_transitions_live -- --ignored --nocapture`) drives the concrete
Anthropic runtime a child resolves to (roster alias for
`claude-oauth:claude-sonnet-5-5`, effort `high`) through the streaming turn
loop: two file-and-arithmetic turns, a real summary of the first turn through
the production draft path (curator plan review, generation on
`claude-sonnet-5-5`, review, apply), a turn, revert, a turn, reapply and a
final turn. With `prefix_mismatch_behavior: "error"` every request that still
replayed a block bound to a changed prefix would be rejected.

Run at 2026-09-30T18:03Z, session
`session_cactus_1790791430588_7b9b6bfbf16d9216`: passed. Every continuation
was accepted, 6 bound thinking blocks were produced, no local or provider
binding event was recorded, and every written value is correct.

| Transition | Suppressed by it | Replayed again | Suppressed after |
|---|---|---|---|
| Review | 1 | 0 | 1 |
| Apply | 1 | 0 | 1 |
| Revert | 2 | 1 | 2 |
| Reapply | 3 | 2 | 3 |

Apply staged exactly what the review showed (the second turn's thinking,
bound to the unsummarized history). Revert replayed it again and suppressed
the thinking produced while the summary was active; reapply did the reverse
and also suppressed the thinking produced after the revert. Two earlier
attempts are recorded: one stopped before the summary because the script
omitted the curator plan review, and one passed every request but produced
thinking in only two requests, so the revert case was not exercised; the
script now asks for arithmetic that needs reasoning.

## WP-04 activation and TUI evidence

Activated build `5cc2be147-dirty-9ab2a70b8f0c` (v0.75.489-dev) through a
coordinated `selfdev build-reload` requested at 2026-09-30T18:15:45Z from a
debug-created selfdev session that was never resumed. The pipeline reported
"published and smoke-tested" with the built source equal to the requested
source. The running server, current and shared-server channels report
`5cc2be147`. As in WP-01 to WP-03, the new server did not load the requesting
session, so the pending activation was completed with
`jcode_build_support::complete_pending_activation_for_session` after a
manifest backup; the canary is `passed`.

An earlier activation of `ddae4736c` served the first tester frames. They
showed that the production review of a ready draft, the curator workspace's
atomic review, did not list the locked group: it was only in the legacy
review lines. Commit `5cc2be147` adds it to the workspace overview, its list
label and its apply overlay; the build above activates that fix.

On the activated build, owned tester clients rendered the debug fixtures with
real key and mouse input, with no frame anomalies:

- 140x48: the review overview shows the locked group (4 invalidated by this
  edit, 1 already invalid, 2 replayed again) and labels the overview item;
  `a` opens the apply overlay stating "Claude thinking: 5 block(s) suppressed
  as invalid, 2 replayed again", and `Esc` closes it without applying. The
  history shows the managed transaction with authorization
  `jcode · Claude thinking invalidation` and a disabled `(Revert)
  (Reapply)`; `r` and `p` refuse with the managed-transaction notice, and a
  mouse click on the disabled Revert opens nothing, while an ordinary
  transaction's `r` still opens the revert confirmation. The managed detail
  names each cause.
- 72x24: the review list labels the overview; `Enter` opens the detail with
  the locked group. The history refuses `r` with the same notice and shows
  the disabled controls.

Frames are kept in the program evidence directory
(`evidence/wp04-2026-09-30/tui-frames/`).

## WP-04 acceptance

Accepted by Mirza on 2026-09-30 (approval received by 18:33Z) on candidate
`c7c6f066a`. Activated runtime `5cc2be147-dirty-9ab2a70b8f0c` (running,
current and shared-server channels equal, canary passed); later commits are
documentation only. Top-level Claude replay remains off until WP-05, which now
has complete invalidation coverage: every context transition and every
request reconciles bound thinking before it is sent.

## Current behavior after WP-05

The current-behavior reference is [`CLAUDE_PROVIDER_PARITY.md`](../CLAUDE_PROVIDER_PARITY.md).
WP-05 changed:

- **Cache placement** (R12, DESIGN §6). `place_cache_breakpoints` marks the
  last system block (covering the tools), the newest content block, the
  previous request's newest block, and an intermediate block when more than
  15 lookback positions separate those two. The last-tool marker and the two
  assistant-anchored markers are removed; `tool_result` and `image` blocks can
  carry a marker. Binding digests exclude every marker, so placement never
  changes what a thinking block is bound to.
- **TTL** (deferred decision, resolved). The existing one-hour default stays
  for every marker. Measured on the downstream owner's logs (7,152
  consecutive request pairs within sessions): 81.7% start under a minute
  apart, 15.4% 1–5 minutes, 2.5% 5–60 minutes, 0.4% over an hour. Writes bill
  only the appended delta, so the one-hour premium is about 0.75x of what a
  session appends, while each 5–60 minute gap under a five-minute TTL rewrites
  the whole conversation at 1.25x. A one-hour system marker with a five-minute
  tail is worse for the same reason: the tail is what gets rewritten.
- **Thinking display** (deferred decision, resolved). `display: "summarized"`
  stays. Current documentation (re-verified 2026-10-01) says it returns the
  reasoning summaries and the between-tool progress notes; `"updates"`
  returns only the notes.
- **Per-model parameters** (R13, DESIGN §7). An explicit default-effort table
  (`anthropic_default_reasoning_effort`): Opus 5.5 `medium`, Opus 5 `low`,
  Opus 4.7/4.8 `xhigh`, earlier Opus `high`, Fable 5/5.1 `high`, everything
  else the model default. Effort `none` follows `anthropic_thinking_off`:
  omitted thinking where omission means none, `{type: "disabled"}` on Opus 5
  and Sonnet 5 (where omission means adaptive thinking), and `low` where
  thinking cannot be disabled (Opus 5.5, Sonnet 5.5, Fable, Mythos), which no
  longer offer `none`. Sonnet 5.5's `between_tools` is not used because it
  rejects `block_binding`. `temperature` is sent only where sampling
  parameters are still accepted (`anthropic_accepts_sampling_parameters`).
  Documentation and measurement disagree here: the documentation removes
  sampling parameters on Opus 5.5, Opus 5 and Sonnet 5 and rejects
  non-default values on Sonnet 5.5, while Gate 0 G0.5 measured `temperature:
  1.0` accepted on all four; omitting it is hygiene.
- **Models.** `claude-opus-5-5`, `claude-fable-5-1` and `claude-sonnet-5-5`
  are in `ALL_CLAUDE_MODELS` (after the default, so fallback ranking prefers
  them over older generations) and priced; jcode-base's `AVAILABLE_MODELS`
  is that list.
- **Claude CLI route** (R15). Each provider instance shows a one-time status
  notice that INT-01 parity does not apply to the deprecated subprocess
  transport.
- **Tool-set lifetime** (carried from WP-04). `jcode_app_core::tool::ToolSetLock`
  is the only owner of a session's locked tool set, for the agent loops and
  the TUI local loop. Context-control transitions, historical tool repair,
  legacy migration, rewind and its undo, and provider or model switches keep
  the set; clear and session changes lock a new one. The set changes only at
  a recorded transition (late MCP registration, MCP tool set reload, tool
  unavailable), each journaled and named as the cause of the thinking it
  invalidates. Before WP-05 every context transition rebuilt the set, so a
  registry change since the lock turned an edit into a full cache break and
  invalidated all Claude thinking, attributed to the edit.
- **Top-level replay** (R14). Top-level Claude sessions replay signed
  thinking; `MultiProvider::reasoning_replay_kind` passes the dispatching
  runtime's kind through.

## WP-05 live evidence (2026-09-30, UTC)

**Top-level legs (R14).** The ignored test `claude_parity_live`
(`src/cli/parity_live_tests.rs`) runs a top-level session on the production
provider (`MultiProvider`, as `--provider claude` builds it, pinned to Claude
OAuth) with a shared-session registry, under
`JCODE_ANTHROPIC_PREFIX_MISMATCH=error`. Each leg is 20 requests: coding turns
with thinking between tool calls, a system reminder, a `get_catalog` plus
`subagent` delegation (the child ran on the `fast-worker` alias), and a real
range summary of the first turn applied through the curator plan review, draft
and apply, then reverted. Run with:

```text
JCODE_ANTHROPIC_PREFIX_MISMATCH=error JCODE_WP05_LIVE_MODEL=<model> \
    cargo test -p jcode --lib claude_parity_live -- --ignored --nocapture
```

| Leg | Effort | Session | Requests | Rejections | Binding events | Cache reads, measured requests | Thinking stored / replayed at the end |
|---|---|---|---|---|---|---|---|
| Opus 5.5, 22:46Z | `medium` (default table) | `session_turkey_1790808363825_9e0f1aee8ffaf220` | 20 | 0 | none | 96.7–99.6% (16 of 16 ≥ 90%) | 8 / 6 |
| Sonnet 5.5, 22:47Z | model default | `session_t-rex_1790808450848_61c35862507eb788` | 20 | 0 | none | 96.7–99.6% (16 of 16 ≥ 90%) | 6 / 5 |
| Opus 5, 22:51Z | `low` (default table) | `session_ladybug_1790808677707_0b0d257d05f012a9` | 20 | 0 | none | 96.7–99.6% (16 of 16 ≥ 90%) | 5 / 5 |

- Measured requests are those from the third on, excluding the first request
  after each declared transition (summary apply, revert). Those were 85.0–85.9%
  after the apply and 96.4–96.6% after the revert, which read the entry the
  pre-summary requests had written.
- On the binding models the apply staged 1 block (invalidated by the summary)
  and the revert replayed it again and staged the blocks produced while the
  summary was active (Opus 5.5: 2, Sonnet 5.5: 1). The difference between
  stored and replayed thinking at the end is that managed set. Opus 5, which
  does not bind, staged nothing (D11).
- Every request replayed the stored signed blocks unchanged and the API
  accepted them under `error`; no local invalid-replay or provider
  `input_transformations` event was recorded.
- The Sonnet 5.5 ledger printed its effort as `none`: with nothing configured
  the runtime then reported the model default as `none`. That was a defect in
  the reported value only (the request left effort to the model); it would
  have made a restored session send an explicit `none`, and is fixed in
  `d128652e3`.
- Each leg's first request wrote about 20.1K tokens (tools, system and the
  first prompt); every later request read the previous request's entry.

**Effort `none` shapes (R13).** `effort_none_shapes_live` sent one request per
model over Claude OAuth at 22:45Z: Opus 5 with `thinking: {type: "disabled"}`
and no effort (`session_hare_1790808346516_2ec113c0073bc6a6`), and Opus 5.5,
where `none` means `low` (`session_vole_1790808350221_5bf4bf9c1378cb90`). Both
were accepted. Gate 0 G0.5 (`temperature: 1.0` tolerated) stays the recorded
evidence for sampling parameters.

Reports: program evidence `evidence/wp05-2026-10-01/`.

## WP-05 deterministic evidence

- **Cache placement.** `jcode-provider-anthropic` `cache_breakpoints::tests`
  (10): static-prefix marker on the last system block or, without a system,
  the last tool; first request; each request of a scripted session reads
  exactly where the previous one wrote, with the cached span byte-identical;
  no marker on thinking blocks; the intermediate lookback marker; parallel tool
  runs counted as one position; idempotence; uniform TTL. The runtime's
  `production_requests_read_where_the_previous_request_wrote` checks the same
  through `build_api_request` with real bindings and asserts every replayed
  block stays valid.
- **Parameters.** `jcode-provider-core`: `default_effort_is_an_explicit_per_model_table`,
  `thinking_off_follows_each_generation`,
  `sampling_parameters_are_sent_only_where_documented`,
  `current_claude_models_are_listed_and_classified`, and the pricing rates.
  Runtime: `effort_none_follows_how_each_generation_turns_thinking_off`,
  `effort_none_is_not_offered_where_thinking_cannot_be_turned_off`,
  `unconfigured_efforts_follow_the_default_table`,
  `an_unconfigured_effort_is_the_model_default_not_none`.
- **Claude CLI route.** `the_cli_route_states_once_that_claude_parity_does_not_apply`
  runs a fake CLI binary through the real subprocess path; the replay-kind
  dispatch test covers a Claude slot served only by the CLI.
- **Tool-set lifetime.** `tool::tool_set::tests` (6) for the owner;
  `agent::reasoning_invalidation_tests::a_context_transition_keeps_the_tool_set_and_earlier_thinking`
  (a context transition keeps the tool bytes and every earlier thinking block,
  with an unchanged and with a changed registry, and an `mcp` release is a
  recorded, attributed rebuild) and
  `a_late_mcp_registration_is_one_recorded_attributed_transition`, both
  through the real streaming loop with the production binding rule; the TUI
  `local_requests_keep_the_locked_tool_set_until_a_recorded_transition`.
- **Replay.** `reasoning_replay_kind_follows_the_runtime_a_request_dispatches_to`:
  top-level Claude replays; the CLI route and other slots do not change.

## WP-05 activation and live smoke

The coordinated `selfdev build-reload` built `3204b4481` and the shared server
reloaded into `3204b4481-dirty-d255ec210806` (v0.75.498-dev) at
2026-10-01T02:55:33Z. The request came from a short-lived self-dev session
that the new server did not load (`session_herb_1790823208829_35e6d983bfddc56a`;
never resume it). After the smoke below, the pending activation bound to it
was completed with `jcode_build_support::complete_pending_activation_for_session`
(manifest backup in `~/.jcode/scratch/int01-wp05-activation-20261001/`).
Running, current and shared-server channels are equal, the canary is
`passed`, and the reload phase is `SocketReady`.

**Activated-runtime smoke** (02:56Z, the shared server, production
`drop_block`): an owned server session (`session_tigress_1790823376963_7e90084725a5b97b`)
on Claude Opus 5.5 over OAuth ran two turns that think between `read` and
`write` calls. Its stored thinking carries bindings, which a top-level
session records only with replay on. The six requests read 21.9K–22.8K
cached tokens after the first (97.9–99.4% of input), and the log shows no
`INV-1` warning, binding event or dropped block.

**TUI evidence and the second activation.** An owned tester client on
`3204b4481` showed, after real `/model claude-opus-5-5` and `/effort` input,
"Available: None · Low ← current · …": the remote client built Claude's
ladder from a second copy of the runtime's logic, and a model change carried
no effort, so the header kept Opus 5's `low`. Both were repaired
(`8d0d48bf3`, one ladder in `anthropic_selectable_efforts`; `3a6012c4c`,
`ModelChanged` reports the effective effort) and activated in a second
build-reload: runtime `3a6012c4c-dirty-ca803dbbbde2` (v0.75.501-dev),
running, current and shared-server equal, canary `passed` (pending activation
completed the same way, request session `session_mizaru_1790824234409_fee1f10ddd01f58a`;
never resume it). On it a fresh tester showed Opus 5 offering
`None · Low ← current · Medium · High · xHigh · Max`, then after the switch
Opus 5.5 offering `Low · Medium ← current · High · xHigh · Max`, and
`/effort none` answering "Reasoning effort → Low". No frame anomalies; frames
in the program evidence directory. A final live smoke on that runtime
(`session_palmtree_1790824428698_535241a865a1a6c5`, Opus 5.5, two thinking
tool turns) read 97.4–99.9% of input from cache on every request and showed
no `INV-1` warning or binding event.

## WP-05 acceptance

Accepted by Mirza on 2026-10-01 (approval received by 07:18Z) on candidate
`ea56fc01b`, with no requested changes; the default Claude model stays
`claude-opus-5` (Mirza picks models explicitly). Activated runtime
`3a6012c4c-dirty-ca803dbbbde2` (running, current and shared-server channels
equal, canary passed); later commits are documentation only. R12–R15 are
closed; R16 is complete for WP-05 and is reconciled at the INT-01 closeout,
together with the one-release legacy tool-name removal and the pricing
cache-write rate under the one-hour TTL.

## Current behavior after WP-06

WP-06 (boundary hardening, added after the closeout's two independent reviews)
changed the behavior below. The current-behavior reference is
[`CLAUDE_PROVIDER_PARITY.md`](../CLAUDE_PROVIDER_PARITY.md), with
[`NOTIFICATIONS.md`](../NOTIFICATIONS.md#operator-rendering) and
[`CONTEXT_CONTROL.md`](../CONTEXT_CONTROL.md#replayed-reasoning-bound-to-its-prefix).

- **Lossless stream** (R17). The response body is framed as bytes
  (`sse_decoder.rs`): strict UTF-8 per complete line, LF, CR and CRLF line
  endings, joined `data:` lines. A turn completes only on `message_stop` with
  every content block closed; anything else is an incomplete response, a
  retryable transport fault that is never stored as a completed turn. A
  malformed known event is a fault. Cancellation stays distinct.
- **Exact suppression planning** (R18). `binding::plan_suppressions` decides
  every replayed block on the request as the formatter builds it after the
  earlier removals (empty-message drop, role merge, tool results first). The
  runtime validates the exact request before sending and hands back a request
  that still holds an invalid block instead of sending it.
- **Provider feedback** (R19). Drops reported in `input_transformations` and a
  rejection that names a block are mapped through the request to stored
  blocks. The block and the replayable run after it join the managed set with
  cause `ProviderReported`, held until the session is cleared. Routing drops
  (`model_binding_mismatch`, `organization_binding_mismatch`) are not
  persisted; see the deviation below. A rejected request is planned again
  once. The "thinking blocks cannot be modified" rejection takes the same
  path.
- **One request plan** (R20). Route, model and model-dependent parameters are
  resolved once per request. A credential-mode change resets the route
  observation. A model fallback is persisted and handed back to the agent as
  a `provider model fallback` transition and a new plan
  (`ProviderRequestReplan`), or rebuilt completely for callers that cannot
  plan again. The reasoning self-heal keeps `block_binding`.
- **Retention across switches** (R21, D16). A route or model that does not
  bind changes nothing in the managed set. On a binding route the set is
  recomputed from bindings as before; provider-reported blocks stack with it.
  Nothing is stripped or restored because of a switch.
- **Durable tool set** (R22, D15). `Session::tool_set` holds the definitions
  the first request advertised and every change announced since. Changes are
  announced once by an appended `ToolSet` delivery; Anthropic models with
  in-message tool changes keep the frozen `tools` array, other runtimes take
  additions and schema changes into the array at a recorded `tool set change`
  transition. `ToolSetLock`, the one-shot late-MCP rebuild and the `mcp`
  unlock are gone.
- **Operator notices** (R23, D17). Deliveries store the authority they ask
  for. Anthropic renders operator deliveries as `role: "system"` messages
  where the model and the placement allow, OpenAI as `developer` messages,
  every other runtime as the stored user text.
- **Effort intent** (R24). Sessions store `Default` or `Explicit(level)`;
  restore, resume, model switches and fallbacks apply it for the current
  model. Stored strings from before migrate without changing the effective
  effort.
- **Defaults** (R25, D18). `claude-opus-5-5` is the default Claude model and
  its default effort is `high`.
- **Tool names** (R26). A tool the dispatching runtime would reject by name
  is not advertised there, with a status notice.
- **Identity and display** (R27). Every thinking configuration asks for
  `display: "summarized"`. A changed billing header between two requests of
  a session is journaled as `OAuth client identity sync`.

## WP-06 boundary probes (2026-10-01)

`jcode provider-doctor claude --contract claude-oauth-boundaries --model <id>`
(`crates/jcode-provider-doctor/src/claude_boundaries.rs`) sends the probes
below with the runtime's production request builder, token resolution and
attribution headers, and writes a redacted report. Route Claude OAuth, native
Anthropic runtime; effort `low` for G6.1a–e and `high` for the thinking
chain; `display: "summarized"`; run 2026-10-01 from 13:46Z. Reports: program
evidence `evidence/wp06-2026-10-01/probes/`.

| Probe | Opus 5.5 | Sonnet 5.5 | Opus 5 | Sonnet 5 |
|---|---|---|---|---|
| G6.1a `role: "system"` message after a user message | 200, followed | 200 | 200 | 200 (documentation says unsupported) |
| G6.1b two adjacent system messages | 200 | 200 | 200 | 200 |
| G6.1c `cache_control` on a system message | 200, cached | 200 | 200 | 200 |
| G6.1d system message after a `tool_result` with thinking, under `error`, then the next request | 200, 200 | 200, 200 | 200, 200 (no binding check) | 200, 200 (no binding check) |
| G6.1e system message text inside the `<system-reminder>` wrapper | 200 | 200 | 200 | 200 |
| G6.2a `tool_addition` by value, new tool (`inline-tools-2026-09-15`), under `error` | 200, cache read kept; next 200 | same | same | 400 "tool_addition/tool_removal is not supported on this model" |
| G6.2b `tool_addition` by value replacing a same-name tool | 200, cache read kept; next 200 | same | same | 400 |
| G6.2c `tool_removal` by name | 200, cache read kept; next 200 | same | same | 400 |
| G6.2d by-reference addition of a deferred tool appended to `tools` | 200, but cache read 0 | same | same | 400 |
| G6.4 U+FFFD in the latest assistant message's summarized thinking, `drop_block` | 200, `[]` | 200, `[]` | 200 | 200 |
| G6.5 Opus 5.5 thinking, two turns on Sonnet 5.5, back on Opus 5.5, under `error` | Sonnet legs report `model_binding_mismatch` for the Opus block; back on Opus 5.5 `[]` on both requests | — | — | — |
| G6.6 billing header version bumped, under `error` | 200, `[]`, cache read unchanged | 200, `[]` | 200 | 200 |

Decisions taken from the results:

- **System messages and tool changes.** Opus 5.5, Sonnet 5.5 and Opus 5 get
  `role: "system"` rendering and in-message tool changes. The by-value inline
  form is used: the by-reference form needs the tool appended to `tools`,
  which breaks the cache. Sonnet 5 accepted a system message although the
  documentation lists it as unsupported; jcode follows the documentation and
  keeps the user form there (the form it always sent), and it rejects tool
  changes, so its array carries them.
- **Wrapper.** The operator form carries the body without the
  `<system-reminder>` wrapper; the wrapper marks harness text inside user
  content and both forms were accepted.
- **Modified thinking.** A changed character in summarized thinking was not
  rejected and nothing was dropped on this account: summary text is not
  verified, so a damaged summary cannot wedge a session. The recovery for
  the documented rejection is implemented and tested against the documented
  error text.
- **Switch and back.** A round trip through a model that cannot read a block
  loses nothing, so routing drops are not persisted as suppressions.
- **Billing header.** The API neither caches nor binds that block. The
  binding digest hashes it as fixed text, so a version sync invalidates no
  thinking.
- **Pending for the closeout.** G6.3 (an OpenAI `developer` item on GPT-5.6
  Sol over OAuth) was not run in WP-06: no OpenAI live calls were allowed.
  OpenAI's `developer` rendering is selected by its deterministic tests.

## WP-06 deterministic evidence

- **R17.** `jcode-provider-anthropic-runtime` `sse_decoder::tests` (every
  byte split of multi-byte text; LF, CR and CRLF; joined `data:` lines; cut
  bodies; invalid UTF-8 is a fault) and `boundary_tests` through a local HTTP
  server and the production `complete` path:
  `multibyte_text_survives_any_chunking_and_both_line_endings_over_http`
  (splits inside text, thinking, signatures and `input_json`),
  `a_body_that_ends_before_the_response_completes_is_a_transport_fault` (EOF
  after every event and inside a block),
  `an_incomplete_response_is_retried_and_partial_output_is_rolled_back`,
  `a_consumer_that_stops_listening_is_a_cancellation_not_a_fault`. With the
  former per-chunk lossy decoding the first test fails.
- **R18.** `jcode-provider-anthropic` `binding::tests`: thinking-only and
  redacted-only turns, `suppressing_a_thinking_only_turn_invalidates_the_thinking_after_it`,
  `thinking_produced_after_a_suppressed_thinking_only_turn_stays_valid`,
  `restoring_a_thinking_only_turn_invalidates_what_was_produced_without_it`,
  `a_turn_cut_off_after_a_closed_thinking_block_replays_it` (the two reviewer
  counterexamples among them); each asserts that no invalid block is sent and
  no valid block is suppressed. Runtime:
  `a_request_whose_plan_resolved_differently_is_handed_back_not_sent`.
- **R19.** Runtime `provider_reported_drops_name_the_block_and_the_run_after_it`
  (in `message_start` and `message_delta`),
  `unknown_drops_are_fed_back_and_routing_drops_are_kept`,
  `a_rejection_of_replayed_thinking_asks_the_caller_to_replan_without_the_named_run`;
  `jcode-app-core` `agent::request_replan_tests`
  (`a_provider_reported_drop_is_persisted_and_not_sent_again`,
  `rejected_reasoning_is_suppressed_and_the_request_is_sent_once_more`) and
  `context::reasoning_invalidation_tests::provider_reported_blocks_join_the_managed_set_with_their_cause`.
- **R20.** Runtime `a_credential_mode_change_decides_the_next_request_route`
  (both directions, through the public setter),
  `a_model_fallback_is_a_complete_new_plan`,
  `the_reasoning_self_heal_keeps_the_binding_control`; app-core
  `a_model_fallback_is_adopted_recorded_and_planned_again`,
  `a_request_handed_back_without_end_fails_instead_of_looping`.
- **R21.** `context::reasoning_invalidation_tests`:
  `switching_to_an_unbound_model_and_back_restores_and_strips_nothing`,
  `an_unbound_model_stages_and_reports_nothing`, and the unchanged WP-04
  revert and reapply cases.
- **R22.** `jcode-session-types` `tool_set::tests`; app-core
  `tool::tool_set::tests`; `agent::tool_set_tests`: a session restarted into
  a fresh Agent and registry keeps its tool bytes on the Anthropic and OpenAI
  builders with an unchanged registry and across two restarts; a delayed MCP
  reconnect, a description change, a schema change, an addition and a
  removal each produce exactly one notice and the documented array behavior,
  for a provider whose array carries changes and for one with in-message
  changes; calls to a reconnecting and to a removed tool are refused with
  their reason; a rewound notice is announced again.
  `agent::reasoning_invalidation_tests`: a context transition keeps tools and
  thinking; an array change is a recorded, attributed transition; an
  in-message change keeps the array and every earlier thinking block valid.
  `jcode-provider-anthropic` `operator_messages_tests`: changes by value and
  by name, moved when the notice is user text, a repeated change rendered
  once. `jcode-tui`:
  `local_requests_keep_the_frozen_tool_set_and_announce_a_change_once`.
- **R23.** `jcode-provider-anthropic` `operator_messages_tests` (placement
  rules, the unanswered-notice flip, thinking bound across a system message,
  cache placement); runtime `operator_notices_follow_each_models_capability`
  (Opus 5.5, Sonnet 5.5, Opus 5, Sonnet 5, Opus 4.7); `jcode-provider-openai`
  developer-item tests; app-core
  `agent::context_delivery_tests::scripted_session_is_append_only_on_both_production_builders`,
  extended to the Anthropic builder with and without system messages and the
  OpenAI developer form, with a failed request after a delivery and a reload.
  Removing the allowance for the unanswered suffix makes it fail at exactly
  that block.
- **R24.** `jcode-session-types` `effort::tests`; app-core
  `agent::effort_intent_tests` (default follows the model across a switch and
  two resumes; a chosen level across switches and resumes; migration of
  stored strings; no inheritance between sessions on one runtime);
  `server::provider_control::tests::a_remote_default_effort_follows_the_model_and_survives_a_restart`
  through the handlers a remote client drives; runtime
  `resetting_the_effort_returns_to_the_runtime_default_for_the_model`.
- **R25.** `jcode-provider-core` `default_effort_is_an_explicit_per_model_table`,
  `quality_first_defaults_are_first_in_curated_model_orders`; runtime
  `the_default_claude_model_is_opus_5_5_at_high_effort`.
- **R26.** `jcode-provider-core` `tool_name_policy::rule_tests`; app-core
  `agent::tool_set_tests::tools_a_provider_would_reject_by_name_are_withheld_with_a_notice`
  (invalid characters, a 100-character and an overlong name, under the
  Anthropic and the OpenAI rule, checked on both production builders).
- **R27.** Runtime tests that every thinking configuration carries
  `display: "summarized"`; `binding::tests` for the fixed billing text in the
  digest; app-core
  `agent::tool_set_tests::a_client_identity_sync_is_a_recorded_transition`.

Deviations from the WP-06 specification, recorded for the closeout:

- **R19, routing drops.** The specification feeds every reported drop back
  into the managed set. `model_binding_mismatch` and
  `organization_binding_mismatch` are left out: they are routing drops that
  the documentation and probe G6.5 show to lose nothing on the way back, and
  persisting them would strip thinking for a switch, which D16 forbids. Every
  other type and reason, unknown ones included, is fed back.
- **R27, identity syncs.** The sync is journaled and kept as a cause label,
  but it invalidates no thinking: the digest treats the billing block as
  fixed text because G6.6 measured that the API does not bind it.
- **Tool-set transition name.** The former `late MCP tool registration`,
  `MCP tool set reload` and `Swarm globally disabled` journal labels are one
  label, `tool set change`; the notice names the tools.
