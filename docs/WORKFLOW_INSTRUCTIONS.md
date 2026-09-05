# Managed workflow instructions

Workflow prose uses the same [instruction runtime](dev/INSTRUCTION_RUNTIME.md) and [Git-backed stores](INSTRUCTION_STORES.md) as agent profiles and notifications. Existing workflow owners retain their triggers, provider/model selection, permissions, task data, framing, persistence, and execution policy. Editing prose does not create or enable a workflow.

## Implemented consumers

The Phase 3 migration is in progress. The authoritative row-by-row status is [the instruction inventory](dev/INSTRUCTION_INVENTORY.md).

### Transfer handoff

`modules/transfer-handoff-task.md` supplies the specialist's user-task instructions. `system/transfer-handoff-system.md` supplies its true system prompt. They render from current working files in the transferring session's project scope, with global fallback. They are not selectable primary-agent profiles.

Empty history needs no source access or specialist call. Invalid or missing selected instructions fail before the specialist call or child publication. The existing conversation formatting and bounded excerpt remain owned by transfer. Managed instructions are never truncated. The complete-input budget correction is under WP-07 review, rather than claimed complete by the initial migration.

### Ambient cycles

`modules/ambient-identity.md`, `ambient-empty-queue.md`, `ambient-directives.md`, `ambient-instructions.md`, and `ambient-cycle-start.md` supply current cycle prose. Ambient cycles have global scope, not the project of any queued task. State, queue, session and resource-budget facts remain code-owned.

All selected prose renders before reply directives are consumed or a specialist is launched. The empty-queue and directive guidance are required only for their respective branches. An invalid required source fails the cycle preparation without consuming replies. Already rendered results do not change after source edits.

Ambient mode remains disabled when `[ambient].enabled = false`. The migration does not enable it, change scheduling, adopt the model roster, or add profile selection. Dormant memory prose remains outside managed instructions and the hard-disabled memory policy is unchanged. Phase 9 owns future unattended execution and retry policy.

### Swarm effort directives

`system/swarm-effort.md` and `system/swarm-deep-effort.md` supply the dynamic suffix selected by the existing effort sentinels. Each request renders current prose in its session project scope. Ordinary reasoning efforts do not access these sources. Static system text, tools, model routing, effort mapping and Swarm execution policy remain unchanged.

Invalid selected prose fails before provider dispatch in app-core and local TUI paths. The TUI uses one rendered instruction snapshot for both request accounting and dispatch. Empty guidance contributes no prose. These resources do not start a worker or enable Swarm.

## Source and failure semantics

Working files are authoritative, including intentionally empty bodies where the owner retains meaningful structure. A present invalid project redefinition fails rather than exposing global prose. Missing previously adopted singleton resources are damage, not permission to recreate defaults. New shipped paths use the existing versioned, scoped Git seed-adoption transaction. It preserves current files and does not push a repository.

An ordinary instruction edit affects the next workflow invocation. It does not rewrite earlier messages, change the frozen primary system prompt, or reactivate a completed workflow. Human prompt quality is reviewed separately from synthetic mechanism tests and one-time migration byte evidence.
