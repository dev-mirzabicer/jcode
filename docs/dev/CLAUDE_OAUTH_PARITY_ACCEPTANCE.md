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
attribution headers, sends about thirteen requests (spending subscription
quota), writes a redacted JSON report (signatures and redacted-thinking data
replaced by their length, no credential material) to
`~/.jcode/provider-contract/` or `--contract-out`, and exits non-zero when an
outcome drifts from the recorded Gate 0 observation.

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

The probe's model list of prefix-bound models is Gate 0 observation data for
comparing outcomes only. Runtime binding policy is owned by the per-model
capability introduced in WP-02 (D11).

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
