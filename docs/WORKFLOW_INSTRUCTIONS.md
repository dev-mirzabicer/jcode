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

### Review and judge startup

`modules/review-startup.md`, `autoreview-startup.md`, `judge-startup.md`, and `autojudge-startup.md` compose the existing startup instructions with the shared `review-read-only-guardrails` and `judge-visible-context` modules. These are workflow modules, not the Phase 5 primary-agent set. Model overrides, parent targeting, judge transcript mirroring and command triggers stay with their existing owners.

Local launches render before cloning. Remote launches use the distinct `SplitWithWorkflow` operation: the server captures the parent once, renders in that parent's scope, then clones that same snapshot. Rendering failure creates no child. A failed fork-notice/save preparation cleans the unpublished child. Dedicated workflow success/failure events and request/session correlation prevent stale replies from launching a window or failing an unrelated model turn. Ordinary split and transfer wire shapes remain unchanged.

Startup banners derive from typed review mode and parent metadata, not prompt prefixes. Their existing persisted hint fields keep old queued starts compatible. Startup payload writes are atomic and failures stop window launch. If an already-created session cannot be prepared, the error identifies it and states that no window was launched. Source text remains separately editable without changing banner identity or silently bypassing the judge mirror.

### Mission continuation

Mission introduction, continuation and generated default intent use managed `modules/mission-*` resources. The mission owner retains XML tags and escaping. User objective and intent data remain literal, including template-looking text. This corrects the former chained-replacement behavior that could replace a placeholder inside the user's objective.

Mission creation and rendering accept an explicit working directory for scope. No mission or an inactive mission performs no instruction-source read. Failed creation preserves the previous stored mission. A local turn with invalid mission instructions preserves its raw composer input, cursor, paste backing and images without appending a user turn. Queued preparation uses the existing instruction-error recovery boundary. The migration adds no new mission UI, remote activation policy, profile selection or automatic context operation.

### Interactive command workflows

Commit/push/release, triage, `/test`, `/plan`, improve/refactor run/plan/stop/resume use `workflow-*` modules and notifications. Shared release and commit sections remain reusable modules. Dynamic focus, goal and todo rows are captured as typed data. One catalog snapshot renders each complete command.

Local commands render before changing loop mode, appending a turn or requesting interruption. Remote commands prepare asynchronously through the server's existing `RenderWorkflowPrompt` operation. The TUI remains responsive and does not load its own instruction store for remote commands. Only after successful rendering does it apply the existing dispatch policy: busy commit/triage/release uses soft interruption, busy plan/improve/refactor cancels and queues, and `/test` queues. Idle send authority, retry settings and Startup Context observation remain unchanged.

Pending preparation is request/session/working-directory correlated and cancellable. Source failures preserve the current mode/turn and retain the command for explicit repair and retry. Reconnect reissues only unresolved read-only rendering. Process reload restores interrupted preparation as unconfirmed, suspended intent, not automatic execution. The UI tells the user to verify whether a command was dispatched before explicitly re-running it. This avoids blindly repeating a commit, push or release after an uncertain interruption.

### Swarm worker and task-control instructions

Worker report, deep-node/gate, assignment, restart, wake, synthesis, salvage, replacement and stand-down prose uses `swarm-*` managed resources. The existing planner/integrator uses its coordinator's scope. Worker and displaced-worker guidance uses their respective member working directories. Data hydration, status/assignment rules, admission limits, scheduling, model selection and delivery channels remain unchanged.

Swarm core retains the structural report/deep markers, wrapper spacing, idempotency and existing bounded-node selector. It accepts lazy render callbacks rather than owning another loader. Already-framed contracts do not read sources again. Assignment contracts render before plan mutation, and stand-down guidance renders before takeover. An invalid source leaves the prior assignment intact. Pre-mutation source failures notify duplicate waiters without caching the failure as a completed mutation, allowing an identical explicit retry after repair. Successful operation replay remains unchanged.

If integration instructions fail after workers completed, the error preserves their outputs rather than implying those tasks were rolled back. No new delegation system or roster adoption was introduced. Phase 4 still owns future isolated delegation and Phase 9 owns future async policy.

### Preferred-tool guidance

Optional `tools/preferred-tools.md` resources contribute global and project guidance independently. Primary activation preserves its approved global-before-project order. Existing non-primary compatibility builders retain their historical project-before-global order without adopting named profiles. Both use the same registered scope-aware renderer, not separate legacy loaders. Project-only consumers never fall back to global and duplicate it.

Global legacy content imports exactly through a committed receipt when the store first adopts this cutover. Originals remain unchanged and inactive afterward. Project legacy content remains a compatibility input until explicitly imported or replaced by a managed project definition. Invalid managed content fails instead of revealing legacy/global text. A missing imported target blocks for repair; clear its body to suppress prose deliberately. Existing imported whitespace normalization and legacy provenance headings remain byte-identical. New managed bodies render completely.

Compatibility prompt APIs now propagate instruction failures before provider use. TUI construction derives initial accounting from installed prompt state instead of reading or initializing a client-local instruction store merely for an estimate.

## Source and failure semantics

Working files are authoritative, including intentionally empty bodies where the owner retains meaningful structure. A present invalid project redefinition fails rather than exposing global prose. Missing previously adopted singleton resources are damage, not permission to recreate defaults. New shipped paths use the existing versioned, scoped Git seed-adoption transaction. It preserves current files and does not push a repository.

An ordinary instruction edit affects the next workflow invocation. It does not rewrite earlier messages, change the frozen primary system prompt, or reactivate a completed workflow. Human prompt quality is reviewed separately from synthetic mechanism tests and one-time migration byte evidence.
