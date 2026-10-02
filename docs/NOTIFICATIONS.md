# Managed notifications and control messages

Jcode's registered notification prose lives in the global instruction repository at `~/.jcode/instructions/notifications/`. Reusable prose may live under `modules/`. A configured project instruction repository can redefine a notification by its stable ID. See [Instruction stores](INSTRUCTION_STORES.md) for repository setup, scope resolution, edits and recovery.

## When edits take effect

Each occurrence renders current working files through the instruction runtime. Editing a notification affects later occurrences, not earlier messages. There is no watcher, source-version comparison or source hash in session notification state. System-prompt and active-skill snapshots remain separate and unchanged.

The subsystem emitting a notification still owns its trigger, role, timestamps, structural tags, escaping, runtime facts, task/receipt IDs, ordering, delivery and retry policy. Editing prose does not change command-risk enforcement, browser readiness, permission detection, Startup Context capture or context-control policy.

Plain text is literal. Resources explicitly using Handlebars accept only their registered values and managed module references. Invalid specific project content fails that occurrence rather than falling back to global. Empty content removes prose where the owning event has meaningful framing or facts. The bare `todo-auto-poke` continuation requires nonempty content because it has no independent wrapper or payload. Jcode blocks that occurrence rather than dispatching an empty provider turn.

## Todo continuations and history

Todo followups carry typed intent and captured assessment data through the queue. Local dispatch renders local sources. A remote TUI sends the typed request to its server, which renders using the recipient session's working directory. Client instruction files are not used for remote notifications.

The provider still receives one ordinary user-authority message with the existing text and double-newline composition. Out-of-band `StoredMessage.origin` metadata identifies control and human portions of that stored text. This metadata does not enter provider messages or change their timestamps. Mixed history displays retain the user's text instead of hiding the whole message behind a todo summary.

Reload, reconnect, retry and accepted provider fallback retain typed queued intent. A rendering failure preserves the queue and stops automatic redispatch. Repair the instruction and explicitly submit a message or use `/poke` to retry. Old saved sessions and queue snapshots without origin metadata retain their historical decoding. Matching current client and server versions are assumed.

Origin ranges survive ordinary session persistence, inheritance and exports. Export redaction remains authoritative. Ranges are rebased over the exact redacted result where possible. If a redaction crosses part boundaries, complete redacted text is displayed rather than trusting stale ranges that could hide user content.

## Delivery of model-visible context

Per-turn system reminders, the batch nudge, reload-resume continuations and tool-set notices reach the model only as persisted transcript content. The same stored text serves every provider, GPT and Claude alike. Nothing per-request is added to the system prompt or inserted into history for one request only, so each provider request is the previous one with content appended at the end.

- **Form.** A delivery is stored as a user-role message, `<system-reminder>\n…\n</system-reminder>`, with `display_role: System` and a structural `ContextDelivery` origin (channel, a fingerprint of the exact text, and the authority it asks for). Turn reminders keep their `# System Reminder` heading. The origin never reaches the provider. A delivery whose text no longer matches its fingerprint (for example after export redaction) is treated as ordinary source.
- **Authority.** Turn reminders, the batch nudge and tool-set notices ask for operator authority; Startup Context and everything a person wrote do not. A runtime with a native operator channel renders such a delivery there, and every other runtime receives the stored text ([Operator rendering](#operator-rendering)).
- **Once per occurrence.** A turn's reminder is committed with the input it accompanies: same save, same durable input receipt, directly after the input's content. Safe-boundary inputs injected together share one delivery, after the group's last input. An input with no content, such as a reload resume, has the delivery as its entire content, so no empty prompt is stored. The batch nudge is delivered when it fires. Identical text on a later occurrence is delivered again; reminders are events, not state.
- **Append-only.** A delivery is never re-sent, rewritten, moved or removed. Later requests carry it as ordinary history, and context control can summarize it like any other message.
- **Static prompt.** Active-skill text and the dormant Swarm effort directive are sections of the static system prompt. A skill activation, and a switch into or out of a Swarm effort, is a recorded prompt transition in the cache-invalidation journal; no other request-time system content exists. The directive renders its managed source on each request, so a future Swarm reactivation must also freeze that text or record its source edits as transitions.
- **Failure.** If the delivery cannot be persisted with its input, the input commit fails and the turn is not dispatched; the existing turn-setup abort preserves the input.
- **History.** Deliveries render as system messages showing their body. Clients render stored messages; there is no separate live event.

### Operator rendering

The stored delivery never changes. On the request path only, a delivery that asks for operator authority is handed to a runtime that declares an operator channel (`Provider::renders_operator_notices`) as an operator notice, and that runtime chooses the native form from its model's capability data and the notice's position:

| Runtime | Rendering |
|---|---|
| Anthropic, model with mid-conversation system messages (Opus 5.5, Sonnet 5.5, Opus 5, Opus 4.8, Fable 5/5.1, Mythos 5/5.1) | A `role: "system"` message with the body, without the wrapper, when it follows a user message and is last or followed by an assistant message. Otherwise the stored user text. |
| Anthropic, other models (Sonnet 5 and older) | The stored user text. |
| OpenAI Responses | A `developer` message in place when enabled (`JCODE_OPENAI_OPERATOR_MESSAGES=1`; off by default until the live probe on the ChatGPT OAuth backend has run). Otherwise the stored user text. |
| Every other runtime | The stored user text. |

The choice is a pure function of the stored history, so it does not change from one request to the next. The single exception is a notice whose request failed before any reply and that a new user message then follows: it becomes user text, which invalidates nothing because no reply came after it. Deliveries stored before authority was recorded keep the user form. Notices are identified by their stored origin, never by their prose. The capability data is `anthropic_conversation_caps` in `jcode-provider-core`; see [Claude provider parity](CLAUDE_PROVIDER_PARITY.md#operator-notices).

### Tool-set notices

A session's tool set is frozen at its first request and persisted ([tool set](CLAUDE_PROVIDER_PARITY.md#tool-set)). When the live registry differs before a later request (a tool added, removed or redefined), the session appends one tool-set notice on the `ToolSet` channel, before that request, and records the change. The notice:

- lists every change in that comparison under the heading `# Tool set changed (update N)`, where `N` numbers the session's notices; a redefinition includes the new description;
- carries the changes structurally in its origin, so a runtime that applies tool changes inside a message (`Provider::renders_tool_changes`) renders them as native tool additions and removals in the notice's operator message and keeps its `tools` array as first advertised;
- is delivered once per change. An unchanged registry, a restart and a resume announce nothing;
- is announced again only when a context summary or a rewind removes it from the history such a runtime sees, because the change would otherwise be undone for the model.

The notice text is produced by code from the changed definitions (`jcode_app_core::tool::tool_set_notice`); it has no managed prose resource. If the notice or the record cannot be persisted, the request is not sent.

Since INT-01/WP-03, GPT sessions keep reminders in history instead of receiving a fresh copy after the latest prompt on every request. Old sessions have no deliveries; an old stored message recognized only by its `<system-reminder>` prefix stays hidden in history.

Agent memory is globally disabled ([Memory policy](MEMORY_POLICY.md)). Its dormant injection still adds a trailing request-only message. Any reactivation must deliver through this persisted path instead, because a message present in one request and absent from the next breaks append-only provider history and, on Claude, invalidates later thinking; it would also carry the request's newest prompt-cache breakpoint, writing an entry the next request cannot read ([cache placement](CLAUDE_PROVIDER_PARITY.md#prompt-cache)).

The text form and origin validation live in `jcode_session_types::context_delivery`; `Session::append_context_delivery` and `Session::append_tool_set_delivery` are the only writers. When to deliver belongs to the owners of each occurrence: the agent input commit, safe-boundary injection and batch nudge in `jcode-app-core`, and the TUI local turn in `jcode-tui`.

## Failure handling

A notification failure is distinct from the operation it describes:

- Before a new control turn or Startup Context batch is accepted, rendering failure blocks it without a partial append. Startup observation failures preserve prior receipts and counts.
- A completed file write, image generation, background-task launch, coordinator election or plan mutation is not falsely rolled back when its explanatory prose fails. Its owner retains the factual result and reports the rendering failure explicitly.
- A blocked command stays blocked even if refusal prose is empty, missing or invalid. Browser actions stay blocked until the readiness mechanism succeeds.
- Scheduled notification rendering fails before publishing a spawned execution. Existing scheduled failure/dequeue policy is unchanged. This is not a new scheduler or retry system.

## Maintainer boundary

The typed consumer catalog and seed assets are under `jcode-base::instruction::notification`. Delivery remains with Session, Agent, TUI, tool and server owners. New eligible behavioral prose belongs in managed resources, not a new independent loader. Generated environment facts, structural labels, provider-required text, tool schemas, self-development mechanics and protected context-curator prompts remain code-owned.

Seed version 14 adds these resources through the existing isolated instruction-store upgrade transaction. Existing working files are preserved. Previously adopted deletions are not silently recreated. The global store is not pushed automatically.

The source-traced inventory and one-time equality evidence are in [the instruction inventory](dev/INSTRUCTION_INVENTORY.md) and [WP-06 migration evidence](dev/instruction-inventory/WP06_MIGRATION_EVIDENCE.md). Permanent tests use synthetic prose to check mechanisms rather than evaluating prompt quality.
