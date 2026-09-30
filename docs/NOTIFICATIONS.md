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

Per-turn system reminders, the batch nudge and reload-resume continuations reach the model only as persisted transcript content. The same bytes go to every provider, GPT and Claude alike. Nothing per-request is added to the system prompt or inserted into history for one request only, so each provider request is the previous one with content appended at the end.

- **Form.** A delivery is a user-role message, `<system-reminder>\n…\n</system-reminder>`, stored with `display_role: System` and a structural `ContextDelivery` origin (channel plus a fingerprint of the exact text). Turn reminders keep their `# System Reminder` heading. The origin never reaches the provider. A delivery whose text no longer matches its fingerprint (for example after export redaction) is treated as ordinary source.
- **Once per occurrence.** A turn's reminder is committed with the input it accompanies: same save, same durable input receipt, directly after the input's content. Safe-boundary inputs injected together share one delivery, after the group's last input. An input with no content, such as a reload resume, has the delivery as its entire content, so no empty prompt is stored. The batch nudge is delivered when it fires. Identical text on a later occurrence is delivered again; reminders are events, not state.
- **Append-only.** A delivery is never re-sent, rewritten, moved or removed. Later requests carry it as ordinary history, and context control can summarize it like any other message.
- **Static prompt.** Active-skill text and the dormant Swarm effort directive are sections of the static system prompt. A skill activation, and a switch into or out of a Swarm effort, is a recorded prompt transition in the cache-invalidation journal; no other request-time system content exists. The directive renders its managed source on each request, so a future Swarm reactivation must also freeze that text or record its source edits as transitions.
- **Failure.** If the delivery cannot be persisted with its input, the input commit fails and the turn is not dispatched; the existing turn-setup abort preserves the input.
- **History.** Deliveries render as system messages showing their body. Clients render stored messages; there is no separate live event.

Since INT-01/WP-03, GPT sessions keep reminders in history instead of receiving a fresh copy after the latest prompt on every request. Old sessions have no deliveries; an old stored message recognized only by its `<system-reminder>` prefix stays hidden in history.

Agent memory is globally disabled ([Memory policy](MEMORY_POLICY.md)). Its dormant injection still adds a trailing request-only message. Any reactivation must deliver through this persisted path instead, because a message present in one request and absent from the next breaks append-only provider history and, on Claude, invalidates later thinking.

The text form and origin validation live in `jcode_session_types::context_delivery`; `Session::append_context_delivery` is the only writer. When to deliver belongs to the owners of each occurrence: the agent input commit, safe-boundary injection and batch nudge in `jcode-app-core`, and the TUI local turn in `jcode-tui`.

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
