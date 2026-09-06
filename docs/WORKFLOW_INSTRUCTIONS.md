# Managed workflow instructions

Workflow prose uses the same [instruction runtime](dev/INSTRUCTION_RUNTIME.md) and [Git-backed stores](INSTRUCTION_STORES.md) as agent profiles and notifications. Existing workflow owners retain their triggers, provider/model selection, permissions, task data, framing, persistence, and execution policy. Editing prose does not create or enable a workflow.

## Implemented consumers

The Phase 3 migration is in progress. The authoritative row-by-row status is [the instruction inventory](dev/INSTRUCTION_INVENTORY.md).

### Transfer handoff

`modules/transfer-handoff-task.md` supplies the specialist's user-task instructions. `system/transfer-handoff-system.md` supplies its true system prompt. They render from current working files in the transferring session's project scope, with global fallback. They are not selectable primary-agent profiles.

Empty history needs no source access or specialist call. Invalid or missing selected instructions fail before the specialist call or child publication. The conversation formatting and bounded excerpt remain owned by transfer. The input budget includes both system and user instructions, the existing output reserve, and any stricter provider route budget. Managed instructions are never truncated. Near-capacity conversations receive a shorter excerpt. Preparation blocks when complete instructions cannot fit.

### Ambient cycles

`modules/ambient-identity.md`, `ambient-empty-queue.md`, `ambient-directives.md`, `ambient-instructions.md`, and `ambient-cycle-start.md` supply current cycle prose. Ambient cycles have global scope, not the project of any queued task. State, queue, session and resource-budget facts remain code-owned.

All selected prose renders before reply directives are consumed or a specialist is launched. The empty-queue and directive guidance are required only for their respective branches. An invalid required source fails the cycle preparation without consuming replies. Already rendered results do not change after source edits.

Ambient mode remains disabled when `[ambient].enabled = false`. The migration does not enable it, change scheduling, adopt the model roster, or add profile selection. Dormant memory prose remains outside managed instructions and the hard-disabled memory policy is unchanged. Phase 9 owns future unattended execution and retry policy.

### Swarm effort directives

`system/swarm-effort.md` and `system/swarm-deep-effort.md` supply the dynamic suffix selected by the existing effort sentinels. Each request renders current prose in its session project scope. Ordinary reasoning efforts do not access these sources. Static system text, tools, model routing, effort mapping and Swarm execution policy remain unchanged.

Invalid selected prose fails before provider dispatch in app-core and local TUI paths. The TUI uses one rendered instruction snapshot for both request accounting and dispatch. Empty guidance contributes no prose. These resources do not start a worker or enable Swarm.

### SDK structured output

Both SDKs use `RenderWorkflowPrompt` on the connected server before each actual attempt. `modules/structured-output.md` and `notifications/structured-output-correction.md` own the prose. The server supplies structural schema/error/previous-response framing in the attached session's project scope. Typed substitutions expose schema and correction data without arbitrary resource selection or file access.

The SDKs retain schema validation, error normalization, previous-response limits, image/event handling and retry counts. TypeScript's existing UTF-16 excerpt limit now avoids splitting surrogate pairs and reports the actual omitted count. Previously that boundary could produce JSON the Rust server could not decode.

The additive `workflow_prompt_rendering` capability is mandatory for these calls. Older servers fail before a model turn, with no embedded-prose fallback. Rendering does not append history or call a model. Invalid sources propagate as request errors, preserving the current session and preventing the affected attempt.

### Overnight workflows

The existing headless supervisor and launching TUI own overnight execution. The migration does not move supervision to a different process, change queues or timing, or adopt profiles/model aliases. In particular, the existing remote-attached TUI overnight mode remains locally supervised and uses that owner's instruction store and working directory.

Coordinator and visible-start prose live in `modules/overnight-coordinator.md` and `overnight-visible-coordinator.md`, with a conditional `overnight-default-mission.md` module. Follow-up resources live under `notifications/overnight-*`. Paths, times, preflight facts and stall counts remain typed caller data. The fixed auto-poke run marker remains code-owned. Supervisor log labels derive from a typed phase, not editable prose.

A failed visible-start render leaves the parent and run publication untouched. Headless preparation propagates rendering errors before constructing the coordinator Agent or calling the model, using the existing supervisor failure record. Follow-up rendering occurs before setting request flags or consuming the poke count. The TUI retains continuation state after a render failure, queues no turn, and reports the error. Repair can be followed by an explicit continuation. Phase 9 retains ownership of future unattended policy.

## Source and failure semantics

Working files are authoritative, including intentionally empty bodies where the owner retains meaningful structure. A present invalid project redefinition fails rather than exposing global prose. Missing previously adopted singleton resources are damage, not permission to recreate defaults. New shipped paths use the existing versioned, scoped Git seed-adoption transaction. It preserves current files and does not push a repository.

An ordinary instruction edit affects the next workflow invocation. It does not rewrite earlier messages, change the frozen primary system prompt, or reactivate a completed workflow. Human prompt quality is reviewed separately from synthetic mechanism tests and one-time migration byte evidence.
