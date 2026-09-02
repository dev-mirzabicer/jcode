# Cross-crate instruction reconciliation

**Source revision audited:** `6860452868b26ae534635a12753c789cd177b3ca`

**Reconciliation date:** 2026-09-02 UTC

**Purpose:** Close the gaps found during Mirza's WP-01 review by repeating instruction discovery across the complete workspace rather than adding only the two reported rows.

## Method

The reconciliation used four independent navigation passes over every tracked Rust crate:

1. Function names containing prompt, reminder, instruction, notice, assignment, continuation, or reflection
2. Imperative prose such as `You are`, `Continue`, `Resume`, `Retry`, `Do not`, `Return`, and `ask the user`
3. Comments and symbols explicitly identifying model-facing, system-prompt, system-reminder, synthetic-user, or tool-recovery text
4. Delivery sinks: true system parameters, `Message::user`, `ContentBlock::Text`, `StoredDisplayRole`, `ToolOutput`, tool errors, startup messages, Swarm assignments, and system soft interrupts

Each high-signal match was traced to its consumer. Inline tests, provider-doctor probes, human-only TUI/status copy, logs, debug payloads, transport data, and schema-coupled validation were separated from production model instructions. Tool schemas were re-enumerated but remain covered by `INS-EXC-014`.

## Confirmed omissions added

The second audit added these managed rows:

- `INS-NTF-034`: destructive-command reflection and catastrophic denial prose from `jcode-command-risk`
- `INS-NTF-035`: macOS computer permission recovery guidance
- `INS-NTF-036`: cross-agent file-activity conflict alerts
- `INS-NTF-037`: Swarm DM/channel/broadcast delivery envelopes and wake reminders
- `INS-NTF-038`: Swarm plan update/proposal/confirmation/approval/rejection notifications
- `INS-WFL-040`: coordinator addendum framing on ordinary task assignments
- `INS-WFL-041`: production task resume and retry guidance
- `INS-WFL-042`: busy-worker task wake instruction
- `INS-WFL-043`: initial queued-worker assignment wrapper
- `INS-WFL-044`: displaced-worker stand-down instruction after takeover

It also added explicit provider/transport exclusions:

- `INS-EXC-025`: text-only-provider image omission marker
- `INS-EXC-026`: OpenAI oversized encrypted-compaction fallback
- `INS-EXC-027`: OpenAI orphan tool-output recovery wrapper
- `INS-EXC-028`: Jade relay user/cancel transport prefixes

## Review-point precision: retry behavior

Mirza's report correctly identified the `jcode-plan` family as omitted, but the exact live retry route differs from the initial hypothesis:

- `build_control_assignment_text(TaskControlAction::Resume, ...)` is delivered through `comm_control.rs` lines 2183-2184.
- The sole production caller's match arm excludes `TaskControlAction::Retry`.
- Live retry instead builds `Retry this assignment.` in `comm_control.rs`, then routes it through ordinary assignment framing.
- The `restart_instruction_prefix(Retry)` branch in `jcode-plan` is currently reachable from tests/public calls but not from the production server caller.

The inventory records the actual delivered resume and retry text. It does not claim the currently unreachable helper branch is provider-visible.

## Delivery-path reconciliation

### Primary system and skills

Rechecked:

- `jcode-base` prompt assets and builders
- App-core and local-TUI prompt composition
- Legacy base replacements, overlays, preferred tools, and `AGENTS.md`
- Skill registries, project overlays, active skill wrapping, and the Swarm tool description
- Dynamic mission and effort directives

No new family beyond the existing rows was found.

### Persisted and synthetic messages

Rechecked:

- Session context, fork, transfer, Startup Context, and generated-image messages
- Response-recovery and tool-loop continuations
- Todo assessment/quality continuations
- Scheduling, background completion, live-turn reminders, and reload interruption
- Configuration, browser, computer-permission, destructive-command, and integration recovery output

The destructive-command and computer-permission rows were new. Other active families were already represented or were schema-coupled validation covered by an exclusion.

### Swarm and task graph

Rechecked:

- Tool routing prompt and effort directives
- Planner/integrator prompts
- Worker report, deep node, deep gate, and composite synthesis contracts
- Ordinary assignment, resume, retry, wake, takeover, and stand-down paths
- Direct/channel/broadcast messages
- Plan proposal/update/approval/rejection flows
- Coordinator promotion messages

This was the largest gap area. The new notification and workflow rows above now cover each Jcode-authored wrapper or directive that reaches a worker/coordinator model. Raw peer/coordinator/task text remains runtime data under `INS-EXC-017` or `INS-EXC-023`.

### Providers and protocol repairs

Rechecked Anthropic, OpenAI, OpenRouter, Gemini, Copilot, Bedrock, Cursor, Claude CLI, managed-Jcode, and web-transport builders.

- Provider-required identities, billing blocks, continuation repairs, missing-output placeholders, schema transformations, function-call guards, and transport envelopes remain explicit exclusions.
- The OpenAI compaction fallback, orphan-output wrapper, and text-only image omission marker were added because their prior broad structural exclusion did not provide enough source-level accounting.
- Claude CLI adds no independent human-readable system instruction. It forwards the Jcode system string and extracted user prompt to the CLI.

### Context control and memory

Rechecked context curator construction, provider validation, context-pressure paths, dormant memory sidecars, reranking, extraction, and injection.

- Context curator prompts remain protected under `INS-EXC-001` through `INS-EXC-003`.
- OpenAI protected replay recovery is now separately recorded under `INS-EXC-026`.
- Context-pressure errors and recovery events are user/client control surfaces, not model instructions, unless the owning unattended workflow already emits an inventoried notification.
- Dormant memory remains excluded by Phase 2 policy.

### Human-only and data-only crates

Workspace crates that define data models, storage, rendering, authentication UI, update behavior, terminal launch/image handling, telemetry, pricing, configuration types, protocol DTOs, side panels, or TUI widgets do not independently create production model instructions. Their human messages remain covered by `INS-EXC-018`; tests and provider probes remain covered by `INS-EXC-019`; desktop remains covered by `INS-EXC-020`.

Notable classifications:

- Catch-up Markdown is a user-facing artifact, not provider history.
- Input-shell result Markdown is pushed only to the TUI display and status notice.
- Swarm dead-worker salvage text is currently a client notification, not a queued soft interrupt.
- Jade relay prefixes frame external user data and retain user soft-interrupt authority.
- Generic tool argument-validation errors remain mechanically tied to their schemas and dispatch contracts rather than managed operating prose.

## Final reconciliation rule

The inventory now names every active family found by the complete cross-crate passes and every explicit exclusion that required source-level accounting. Later packages migrate by row. If a later source trace finds another genuine starting-revision production instruction, it must treat that as an inventory defect, add the row with evidence, and update this reconciliation rather than silently migrating it.
