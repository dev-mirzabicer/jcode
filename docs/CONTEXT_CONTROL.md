# Context Control and the Context Editor

This document describes Jcode's current user-controlled provider-context subsystem.
It is both a user guide and the authoritative maintainer reference for the subsystem's
product invariants, ownership boundaries, provider caveats, migration behavior, and
lifecycle rules.

For the reproducible requirement matrix, validation commands, performance method, and
latest acceptance results, see [`dev/CONTEXT_CONTROL_ACCEPTANCE.md`](dev/CONTEXT_CONTROL_ACCEPTANCE.md).

## Core model

Jcode has one authoritative transcript:

```text
Session.messages
```

Context control never rewrites or deletes that transcript. Instead, every provider
request is derived from:

```text
authoritative stored messages + persisted context-view transactions
```

The result is a provider-facing projection. Normal history, session search, rewind, and
provenance continue to operate on the authoritative source messages. Export also starts
from authoritative session state and separately redacts sensitive fields.

A context transaction can contain any combination of:

1. A summary of one or more selected, structurally closed message ranges.
2. Suppression of explicitly targeted provider-replayed reasoning blocks.
3. Distillation of selected tool results to strictly less than 20 percent of their
   original estimated provider tokens.

One accepted transaction creates one context revision, one persisted state transition,
and one intentional provider-cache/continuation transition. Transactions can be
reverted and reapplied without reconstructing source content because source content was
never removed.

## Non-negotiable invariants

Future changes must preserve all of these unless product requirements explicitly change:

- `Session.messages` is the sole authoritative transcript.
- Context optimization never mutates or erases authoritative transcript content, including
  stored tool results, reasoning, images, and provider-replay blocks. Provider continuation
  is deliberately invalidated only after a committed historical view change.
- The provider view is deterministic from the transcript and persisted context state.
- A tool call is never split from its result by a selected summary range.
- Context operations are explicit, reviewed, atomic, reversible, and provenance
  preserving.
- Interactive context pressure blocks. It never triggers automatic context mutation,
  native provider compaction, raw history dropping, result truncation, or image deletion.
- Curator work is isolated from the main coding provider and never appears in the coding
  transcript.
- Every independent range summary and every independent tool-result evaluation uses its
  own curator call.
- Curator source is complete. Jcode fails honestly instead of silently shortening,
  capping, pre-summarizing, or replacing required source with omission markers.
- Replayed-reasoning suppression is provider aware. Transcript-only reasoning and
  provider-required function-call signatures are not counted as removable context.
- Persisted targets use stable message identity, block locators, exact hashes, structural
  closure, and strict validation. The system never guesses a nearby replacement target.
- A fully redundant operation is a visible no-op. It does not create a transaction,
  revision, persistence write, cache reset, or continuation reset.
- Explicit unattended authorization is bounded, visible, persisted, and scoped. It is
  never inferred from processing mode, scheduling, ambient mode, or overnight execution.
- Cache economics use the earliest changed provider item and affected suffix. They do
  not call the whole prompt invalidated when a reusable prefix remains.
- Unknown pricing, billing, cache warmth, output behavior, and provider support remain
  unknown. Jcode does not invent dollar precision or validation evidence.
- This subsystem is not a workflow classifier, policy language, persistent curator
  hierarchy, deterministic semantic summarizer, or second transcript.

## Commands

| Command | Behavior |
|---|---|
| `/compact` | Opens the Context Editor in edit mode. It never starts an automatic rewrite. |
| `/context edit` | Opens the same edit workflow. |
| `/context history` | Opens persisted context transaction history. |
| `/context restore` | Opens history for deliberate restoration/reversion review. It does not blindly discard provenance. |
| `/context undo` | Opens the latest-active-transaction undo flow with confirmation. |
| `/context` | Shows runtime, prompt composition, context budget, context revision, transaction counts, and authoritative message counts. |

Legacy `/compact mode ...` commands are retired. They return migration guidance and do
not change state.

## Context Editor workflow

The editor uses typed local/remote protocol state rather than terminal line positions.
Selections are based on stable stored-message IDs and exact block metadata.

### Principal controls

The footer always shows the controls available in the current phase. The main editing
controls are:

- `s`: set the first summary-range endpoint, move, then press `s` again to preview the
  range.
- `Space`: toggle the current message for manual range-oriented actions.
- `R`: open replayed-reasoning controls.
- `d`: toggle the exact current tool-result block.
- `D`: mechanically scan for large older tool-result candidates.
- `C`: open the curator workspace.
- `P`: inspect or change unattended emergency authorization while the session is idle.
- `g`: prepare the exact transaction review.
- `H`: open transaction history.
- `v`: inspect the active operation or summary that owns the selected source item.
- `/`: search loaded authoritative history without losing stable selections.
- `Tab`: rotate History, Preview, and Operations focus.
- `Esc`: unwind the current nested action before closing the editor.

Keyboard and mouse paths use the same state transitions. Narrow terminals retain every
operation through stacked layout, scrolling, and wrapped actions.

### Range selection and structural closure

Range direction is user-interface intent, not chronology. Selecting message 100 and then
message 50 creates the same canonical interval as selecting 50 and then 100.

Before a range can be staged, Jcode computes structural closure. It expands endpoints as
needed so the range does not split:

- matching tool calls and results,
- parallel tool groups,
- result-associated images,
- provider function-call thought signatures,
- or existing active summary boundaries.

The closure preview identifies the final stable endpoints and every automatic expansion.
The user must accept the closed range explicitly.

Active summaries cannot overlap. A new summary may shadow block-level operations only
through the explicit reviewed shadowing path.

### Replayed-reasoning suppression

The primary action is "keep the latest `b` assistant turns." Jcode resolves that request
at draft time into explicit stable block targets. Future appends do not cause a dynamic
rule to suppress additional history silently.

Repeated use is idempotent:

- newly eligible blocks are staged,
- already active equivalent targets are reported as already satisfied,
- material covered by summaries is reported separately,
- protected recent turns remain protected,
- non-replayable material is identified,
- and a fully redundant request produces a non-applicable no-change review.

### Tool-result distillation

Marking or scanning a tool result only selects it for curator evaluation. It does not
claim that the result is safely compressible.

A proposal is eligible only when the independent distiller concludes that continued work
will lose no meaningful information and Jcode re-tokenizes the replacement to strictly
less than 20 percent of the preserved `ToolResult` structure's original estimate.
Exactly 20 percent is rejected.

Eligible proposals are selected by default in final review. The user may deselect any
proposal, and Jcode recalculates the projected tokens and cache effects without another
curator call. Ineligible results remain visible with their reason and uncertainty.

### Active-operation coverage and provenance

Authoritative History shows persisted operations where the next range is selected:

| Marker | Meaning |
|---|---|
| `◆S` | One-message active summary. |
| `╭S`, `│S`, `╰S` | Exact active summary start, interior, and end. |
| `↑S`, `↓S`, `↕S` | The active summary continues beyond the loaded page in the indicated direction. |
| `⋮S` | Continuity resumes across a search or visible stored-index gap. |
| `?S` | Legacy or malformed metadata cannot prove exact coverage. The UI fails closed. |
| `R` | An exact provider-removable reasoning block has active suppression. |
| `D` | The exact tool-result block has an active distillation. |
| `Σ` | A newly staged summary range, not an already active one. |

Coverage is computed globally before pagination. It uses transaction ID, operation index,
stable endpoint IDs, global stored indices, and exact message count, never terminal row
positions.

Press `v` or select Provenance to open the exact owning transaction and operation. Returning
restores the originating stable message and block when they still exist. If the source
changed, Jcode refuses to retarget another block silently.

## Curator architecture and user control

The curator is request scoped. It is not a persistent agent type and it is not exposed to
the coding agent as a context-maintenance tool.

### One task per call

Each semantic task is isolated:

- three selected ranges create three range-summarizer calls,
- five selected tool results create five distiller calls,
- and selecting all eight creates eight independent calls.

A range call receives only its one complete closed range plus its harness-generated file
evidence and applicable instructions. A tool call receives its one complete result, the
matching call, the exact supporting conversation selected by task-scope semantics,
and active summaries relevant to that result. It does not receive other selected targets.

All successful artifacts remain unapplied until the complete transaction passes generation,
identity, provider, projection, review, and persistence gates.

### Complete-source guarantee

For each atomic task, Jcode constructs the complete source first and then checks:

- request-byte limit,
- provider safe-input budget,
- image capability,
- response-byte limit,
- timeout,
- cancellation,
- and strict response schema.

If the complete source does not fit, preparation fails. Jcode does not truncate messages,
shorten tool-result heads or tails, pre-summarize source, drop images, or substitute a
marker while claiming the artifact represents the full source.

Provider-opaque encrypted state and signatures are represented by presence metadata when
the curator does not need the opaque value. Image payloads remain real image blocks for a
curator route that supports image input.

### Prompt transparency and exact-plan identity

Before generation, Exact Calls exposes for every task:

- curator role,
- resolved provider, route, model, and effort,
- effective system instructions,
- response contract,
- authoritative primary source,
- supporting conversation,
- active summaries,
- harness-generated file evidence,
- transaction-wide and task-specific instruction sizes,
- complete-source input estimate, byte size, image count, and safe budget.

The exact plan fingerprint binds session ID, context revision, transcript digest, route,
model, effort, prompts, source scope, evidence, instructions, limits, and task identities.
Generation cannot silently diverge from the reviewed plan.

The approved range-summarizer base prompt is product-controlled and versioned. User
instructions may add precision, but they cannot disable the mandatory information-
preservation and response-contract requirements.

### Per-run settings and saved defaults

The curator workspace can temporarily override provider, route, model, and effort for one
preparation run. It also supports:

- one transaction-wide instruction,
- one additional instruction for each selected range,
- exact effective-prompt inspection,
- restoring the configured default,
- and explicitly saving the current route selection as the default.

Per-run selections and instructions do not become durable merely because they were used.
Saving defaults stores only route configuration. It never stores transcript source or
range-specific instruction text.

The durable configuration is:

```toml
[context.curator]
provider = "openai"       # optional provider selector
route = "openai-oauth"    # optional exact route selector
model = "gpt-5.6-sol[1m]" # optional model/profile identity
effort = "high"           # optional reasoning effort
```

Every field is optional. With no configured fields, Jcode uses an independent fork of the
active provider and model when that fork is suitable. Saving through the Context Editor
patches only these owned keys, validates the complete config, preserves comments and
unknown compatibility keys, writes durably, and reports failure instead of leaving the UI
and disk silently inconsistent.

## Persisted operations and transaction history

Context state stores schema version, revision, emergency policy, transactions, operations,
artifacts, exact source targets, economics, provider-validation evidence, curator usage,
and append-only status events.

A transaction's latest status determines activity:

- applied,
- reverted,
- reapplied,
- or invalidated by a transcript edit.

History is provenance. Transactions are never deleted merely because they were reverted.
Reapply repeats strict target, projection, and provider validation against the current
transcript.

### Atomic commit sequence

Before activation Jcode:

1. verifies draft/session/revision/transcript/provider/route identity,
2. resolves the selected distillation subset,
3. rejects an empty effective operation set,
4. validates persisted state and exact source targets,
5. builds old and proposed projections,
6. runs the active provider's production request-builder adapter,
7. calculates cache-prefix and token economics,
8. records provider-validation evidence,
9. validates the final state again,
10. persists the complete transition,
11. then publishes one context-change reset to cache accounting and provider continuation.

If persistence fails, the previous in-memory context state and provider session identity are
restored. No continuation reset is emitted. Apply races reserve a draft so only one client
can commit it.

## File evidence and summary provenance

Each generated range summary can persist three independent categories:

1. Files changed.
2. Files read or inspected.
3. Paths searched or browsed.

Each category contains sorted, deduplicated paths, a completeness flag, and explicit
warnings when incomplete.

The extractor examines exactly the selected authoritative range and correlates structured
`ToolUse` blocks with exactly one successful matching `ToolResult`. It never reads the live
filesystem.

Key evidence rules:

- successful `write`, `edit`, and `multiedit` calls can prove changed paths,
- successful parsed patch headers can prove added, updated, deleted, and moved paths,
- successful `read` calls can prove files read or inspected,
- Agentgrep search scopes prove only searched/browsed paths,
- Agentgrep outline of one exact file additionally proves inspection,
- directory listings and explicit browser/web/open locators prove browsing, not reading,
- searches never imply every result was read,
- failed, missing-result, or duplicate-result calls never become successful evidence,
- failed mutations can be incomplete because partial effects are possible, but no changed
  path is claimed,
- batch subcalls are correlated independently and fail closed when headers are ambiguous,
- shell commands are opaque and make all categories incomplete,
- unknown tools make all categories incomplete,
- and evidence bounds never silently convert omitted paths or warnings into a false claim
  of completeness.

The range summarizer receives the complete conversation slice as authoritative primary
source. File evidence is supporting metadata. Its mandatory prompt requires preservation of
the substantive contents and findings of file reads, not merely filenames.

The projected summary keeps curator prose and harness evidence distinct so the continuing
agent can distinguish model-authored memory from auditable structured provenance.

## Startup Context source messages

Startup Context control, exact file, late-update, and stale-marker messages are ordinary
authoritative `Session.messages` source. `/context` and the Context Editor read those
messages directly, not the receipt-only remote History or export projection. Exact content
remains inspectable through the normal bounded message-detail path.

A normal explicit range summary may include Startup Context messages. Applying the
summary changes only the provider-facing context view. It does not mutate the original
messages, the immutable captured file bodies, or their receipt-owned structural IDs.
Revert and reapply use the same validation, revision, provenance, continuation-reset, and
cache-transition behavior as every other context transaction. There is no Startup
Context-specific closure rule or second projection system.

See [`STARTUP_CONTEXT.md`](STARTUP_CONTEXT.md) for capture, receipt, lifecycle, History,
export, and privacy behavior.

## Context pressure, safe budgets, and prompt preservation

Jcode computes preflight after assembling the actual system prompts, tools, projected
history, any enabled dynamic context sources, pending user input, and provider request-budget
semantics. Agent memory is globally disabled in Mirza's downstream; see
[`MEMORY_POLICY.md`](MEMORY_POLICY.md).

Provider budgets distinguish:

- `input_only`: subtract only the estimator margin,
- `input_plus_output`: subtract the actual requested output cap and estimator margin,
- `unknown`: do not invent an output reserve; retain the uncertainty and provider-rejection
  recovery path.

The estimator margin is five percent for small windows, bounded to 256 through 4,096 tokens.

Pressure presentation uses the advertised context window:

- Normal: remaining context is greater than both 10 percent and 24,000 tokens.
- Notice: remaining context is at or below `max(10%, 24,000)`.
- Urgent: remaining context is at or below `max(5%, 12,000)`.
- Blocked: projected input is greater than the safe input budget.

Notice and urgent states are UI guidance only. They do not mutate context.

### Blocked interactive requests

When authoritative preflight blocks before a provider call:

- the pending authoritative turn is rolled back durably,
- the exact prompt, pasted content, images, and attachments are restored when their
  correlated identity still matches,
- an occupied composer is never overwritten,
- no context operation is applied,
- and the user must submit manually after editing context.

Jcode never automatically resends a blocked prompt.

If the provider rejects size despite preflight, Jcode distinguishes token-context errors
from HTTP/payload-size errors. Before output, it restores the unanswered prompt when safe.
After output begins, it preserves partial authoritative output instead of pretending the
turn was unsent. HTTP 413 handling preserves images and attachments and never strips or
resends them automatically.

If durable rollback itself fails, Jcode reports that the pending turn remains in
authoritative history and explicitly warns not to resubmit it.

## Explicit unattended emergency authorization

The default emergency policy is `block` for sessions and scheduled tasks.

An authorized policy stores:

- protected recent assistant-turn count,
- target free-context percentage,
- booleans allowing reasoning suppression, tool distillation, and oldest-range summary,
- and bounded authorization provenance.

The Context Editor `P` workflow changes the session policy only while idle. The schedule
tool can attach a policy to one task:

```json
{
  "mode": "authorized",
  "protected_recent_assistant_turns": 5,
  "target_headroom_percent": 10,
  "allow_reasoning_suppression": true,
  "allow_tool_distillation": true,
  "allow_oldest_range_summary": true
}
```

For authorized mode, omitted parameters default to the values above. The protected-turn
count is bounded to at most 1,000 and target headroom must be from 1 through 99 percent.
Omitting the policy or selecting `block` never authorizes surgery.

A dispatched unattended task carries an exact copied authorization. Scheduler restart,
later session-policy changes, or child creation cannot silently alter that task's authority.

When an authorized unattended request cannot fit, Jcode may prepare one transaction in this
order:

1. suppress eligible replayed reasoning outside protected recent turns,
2. distill eligible selected tool results,
3. summarize the minimum oldest structurally closed range needed for target headroom.

The pending prompt, recent protected assistant turns, complete active tool pair, raw
transcript, and provider-required state remain protected. After one successful transaction,
Jcode retries once from a fresh provider context. Failure, insufficient safe reduction, or
a second limit blocks without another transaction or destructive fallback.

Every emergency transaction records authorization, trigger, provider error when relevant,
fit and target reductions, achieved reduction, protected message count, operation order,
and retry outcome. Privacy-safe UI exposes effects without leaking free-text source.

## Cache economics and one-transition behavior

Let `p` be the longest unchanged provider-item prefix. Jcode records:

- the earliest changed provider item `p`,
- old affected suffix tokens,
- new affected suffix tokens,
- total provider-input tokens removed,
- whole-request estimates when available,
- and context headroom gained.

Economics are based on the affected suffix, not automatically the whole prompt. A later
operation cannot move cache invalidation earlier than the earliest already selected change.

For a warm metered route, Jcode compares the old cached request with the new request,
pricing the stable prefix as a read and the rebuilt suffix at a cache-write rate when one is
authoritatively available, otherwise at uncached input. It computes recurring cache-read
savings only when the new suffix is cacheable and both rates are known.

For a cold route, it compares complete old and new rebuilds and records that assumption.
Unknown warmth omits transition dollars. Subscription and included-quota routes report
tokens, headroom, prefix change, and curator usage without fabricated per-request dollars.

If the first request is more expensive and recurring savings are positive:

```text
break-even turns = ceil(first-request penalty / recurring savings per turn)
```

Curator usage and cost are recorded per atomic curator call and remain separate from coding
turn usage and cache-transition estimates.

GPT-5.6 Sol direct-API pricing is tier aware. The current pricing snapshot changes input,
output, and cache-read rates when whole-request input exceeds 272,000 tokens. OpenAI does
not expose a separately authoritative cache-write price in this static contract, so rebuild
cost falls back to uncached input. Tiered dollar estimates are omitted when whole-request
token evidence cannot prove the applicable bands.

## GPT-5.6 Sol context profiles

Jcode exposes two explicit direct-OpenAI profiles over one upstream model:

| Picker name | Persisted Jcode model ID | Context window | OAuth safe input budget | Selection behavior |
|---|---|---:|---:|---|
| GPT-5.6 Sol | `gpt-5.6-sol` | 372,000 | 367,904 | Default and automatic-preference profile. |
| GPT-5.6 Sol (1M) | `gpt-5.6-sol[1m]` | 1,000,000 | 995,904 | Explicit opt-in long-context profile. |

Both send upstream model slug `gpt-5.6-sol`. The `[1m]` suffix is a Jcode profile identity
retained in picker, session, and config state and stripped only at the final OpenAI request
boundary.

The two context windows are authoritative before dynamic catalog fallback, so a live catalog
cannot collapse them. The 1M profile inherits the base model's OpenAI Responses behavior,
reasoning-effort ladder and low default, image capability, continuation transport, and
pricing identity.

The profile is synthesized for direct OpenAI OAuth when the account exposes the base Sol
model and is available to ordinary direct OpenAI API-key routing. It is not a Pro-only
model. Jcode does not fabricate an `openai/gpt-5.6-sol[1m]` upstream model on OpenRouter,
and the managed Jcode subscription catalog remains a separate curated surface.

Selecting the 1M profile changes budgeting and adaptive tool-output protection. It does not
introduce Codex-style automatic history compaction or `model_auto_compact_token_limit`
behavior.

## Provider validation and caveats

Before apply, Jcode validates the complete projected message list through the active
provider runtime's production request builder. A provider with no trustworthy adapter fails
closed.

| Provider family/route | Current validation and operation boundary |
|---|---|
| Native Anthropic | Validates both API-key and OAuth formatter variants as applicable. Signed `AnthropicThinking` can be suppressed only as a complete block. Tool/result and image normalization must still pass. |
| OpenAI Responses | Validates through the Responses input builder. Complete `OpenAIReasoning` items can be suppressed. Every historical context revision clears persistent WebSocket/`previous_response_id` continuation before the next full request. |
| ChatGPT web conversation route | Projected-history operations are disabled because this route does not use the Responses input builder and has no dedicated validation adapter. |
| OpenRouter and named OpenAI-compatible routes | Validates with the exact model-dependent chat formatter options. Generic `Reasoning` is removable only when that route actually replays it. Locked model state fails closed rather than assuming a fallback shape. |
| Native Gemini and Antigravity | Validate through the Gemini contents builder. Range summaries and distillations are supported when the builder accepts them. `ToolUse.thought_signature` is provider-required state and is never a reasoning target. |
| Amazon Bedrock | Validates through the Converse builder only when the AWS SDK feature is present. Replayed-reasoning suppression is not claimed for unsupported block kinds. |
| GitHub Copilot | Validates through the Copilot chat builder. Locked model state fails closed. Copilot does not accept `[1m]` model profiles. |
| Cursor Agent | Validates through the exact text-prompt builder and rejects projections that would depend on silent prefix truncation. Replayed-reasoning suppression is not claimed. |
| Claude CLI route | Historical context operations are disabled. This deprecated route sends the latest prompt and relies on opaque upstream `--resume` state, so it cannot validate replay of edited history. Use native Anthropic. |

Request-builder validation is necessary but not equivalent to a credentialed live provider
probe. The latest source-builder and live-probe status is recorded separately in the
acceptance ledger.

Anthropic model-not-found fallback also honors decimal server recommendations such as
`Please use Opus 4.8.` without treating the version separator as a sentence boundary. The
recommended model must still exist in the known non-retired catalog; otherwise the normal
quality-ranked fallback applies.

## Session lifecycle

### Persistence and reload

Context state round-trips through snapshots, journal metadata, startup stubs, and remote
startup. Any context-view metadata change forces snapshot semantics where required so an
optimistic startup view cannot present stale context state.

### Split and continuing child sessions

True session continuations clone authoritative messages and context state. Parent and child
then evolve independently. Reverting a child transaction does not modify the parent.

Self-dev, scheduled resume, overnight, and other explicit continuation paths clone context
state only where their documented child semantics represent continued history. An exact
scheduled emergency policy remains attached to the dispatched task.

### Transfer

Transfer creates an authoritative handoff message in the child. The child starts with
default context-view state instead of a summary transform whose source messages do not
exist in that child.

### Rewind and rewind undo

Rewind snapshots messages and context state. Transactions whose targets disappeared are
marked transcript-invalidated at one new revision; valid older transactions remain active.
Rewind undo restores the exact prior messages and context state and validates the restored
projection before publication.

When a session's current agent is delivered by an appended profile message, that exact
message is active configuration as well as authoritative transcript source. The Context
Editor marks it as locked, keeps complete detail inspectable, and rejects a summary range
covering it before curator work. Rewind pins it at the new tail boundary rather than
time-traveling agent configuration; undo restores prior history without duplicating it.
Superseded profile messages have no active lock and follow ordinary historical context rules.

### Clear

Clear starts with no context transactions and invalidates provider continuation.

### Provider or model switch

A switch reprojects and revalidates active operations against the new production request
builder, reseeds context budgeting, resets client cache tracking, and invalidates provider
continuation. It never alters raw source content to force compatibility.

### Remote clients

The server remains authoritative. Context snapshots, pagination, drafts, progress, history,
and transaction outcomes are correlated by session, context revision, transcript digest,
request/draft IDs, and client editor epoch. A remote client may release its local transcript
copy after authoritative server startup; this is a client memory optimization, not context
surgery.

## Legacy migration and compatibility

`StoredCompactionState` and `ContentBlock::OpenAICompaction` remain readable only for old
sessions/imports. No current runtime path writes new automatic or native compaction state.

On authoritative load:

- a non-empty legacy text summary with a valid, structurally closed prefix is imported as
  one reversible `LegacyMigration` range-summary transaction,
- the original summary text and legacy coverage metadata are preserved,
- encrypted-only or empty native state restores the full raw transcript rather than
  inventing readable content,
- invalid counts, non-closed ranges, or projection failures restore raw context with an
  explicit migration issue,
- a pre-existing imported migration is retired idempotently,
- and provider continuation is cleared.

If restored raw history does not fit, the next request blocks and requires explicit context
editing. Migration never solves the problem by dropping source.

Obsolete `[compaction]` config keys and OpenAI native-compaction keys remain permissively
readable under Jcode's unknown-field compatibility policy, but they are ignored, omitted
from serialization and environment fingerprints, and cannot reactivate old behavior.
Legacy wire requests receive typed migration rejections.

Old session repair must remain evidence driven. Back up the complete session, verify the
message-array identity, validate any recovered context state strictly against those exact
messages, and write atomically. Do not infer a historical writer or synthesize context state
without direct persistence evidence.

## Privacy, export, and diagnostics

Generated summaries, file evidence, replacements, rationales, instructions, authorization
sources, and provider errors can contain source secrets.

Export redaction scrubs generated transaction text, evidence paths and warnings, curator
provenance text, and emergency free text while leaving the saved session unchanged. After
redaction, category paths and warnings are sorted and deduplicated again so secret collapse
to one marker cannot invalidate strict state.

Ordinary logs and safe debug state expose bounded metadata such as IDs, counts, revisions,
operation kinds, token totals, status, and validation outcome. They must not include raw
curator payloads, prompts, summaries, replacements, evidence paths, encrypted content,
signatures, or source instructions.

## Architecture and source ownership

The subsystem intentionally has one implementation per concern:

| Layer | Ownership |
|---|---|
| `jcode-session-types/src/context.rs` | Persisted provider-neutral contracts, transactions, operations, targets, evidence, economics, curator provenance, emergency policy, and audit records. |
| `jcode-context-core` | Pure target resolution, structural closure, projection, validation, lifecycle reconciliation, estimation, pressure, and cache economics. No storage, provider calls, logging, orchestration, or UI. |
| `jcode-base::Session` | Authoritative messages, context state, projected-message cache and append fast path, persistence/migration integration, memory accounting, and export redaction. |
| `jcode-app-core/src/context` | Snapshot, draft, curator, provider-validation, commit, history, preflight, and file-evidence orchestration. |
| Provider runtimes | Exact production request-builder validation, context-window semantics, and continuation invalidation. |
| `jcode-protocol/src/context.rs` | Bounded typed local/remote DTOs and correlation identities. |
| `jcode-tui::context_editor` | Presentation state machine, stable selection, review/history/provenance, keyboard/mouse handling, and responsive rendering. |
| `context_editor/curator_workspace.rs` | Per-run route/instruction control, exact-call disclosure, default save/restore, progress, outcomes, and atomic retry presentation. |
| `agent/emergency.rs` and scheduler integration | Explicitly authorized unattended planning, protection, one transaction, one retry, and audit. |

Do not duplicate projection, validation, transaction, or evidence semantics in the TUI.
Do not move curator administrative prompts into the coding-agent request path.

## Maintainer change checklist

Before changing this subsystem:

1. Identify which invariant and persisted/protocol contract is affected.
2. Audit local and remote paths together.
3. Preserve stable IDs, exact hashes, direction normalization, structural closure, and
   provider-builder validation.
4. Preserve no-op semantics and one-reset atomicity.
5. Preserve complete curator source and one-task-per-call isolation.
6. Preserve prompt and attachment restoration without automatic resend.
7. Preserve default-block emergency authorization and exact scheduled provenance.
8. Preserve legacy read compatibility without reviving obsolete writers.
9. Update this document and the acceptance matrix with every changed public behavior.
10. Run the exact focused, broad, performance, provider, visual, privacy, migration,
    build/reload, and repository gates in the acceptance ledger.

Do not redesign accepted interaction architecture merely because a nearby implementation is
being refactored. Require direct contradictory product or provider evidence before changing
user-visible semantics.
