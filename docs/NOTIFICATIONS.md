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
