# Agent profiles and frozen system prompts

Jcode primary sessions use one named agent and one exact stored system prompt.

## Initial selection

Selection precedence for a new primary session is:

1. An explicit selector from `jcode --agent <selector>`, remote `Subscribe.agent`, Harness `CreateSession.agent`, or the pre-dispatch `/agent <selector>` command
2. `default_agent` in the configured project instruction store
3. `default_agent` in the global instruction store
4. The effective unqualified `jcode` compatibility agent

Selectors accept `name`, `global:name`, or `project:name`. Unqualified selectors use project-first specificity. A configured or explicit missing, invalid, ambiguous, or primary-unavailable agent fails. Jcode does not continue to a lower selection tier after a configured tier fails.

The Rust SDK exposes `create_session_with_agent`. The TypeScript SDK accepts the optional second `createSession(workingDir, agent)` argument.

## Compatibility agent and kernel

The shipped global instruction seed contains:

- `system/kernel.md`
- `system/common.md`
- `system/mermaid.md`
- `agents/jcode.md`

`agents/jcode.md` is the compatibility profile. Its rendered body is byte-identical to the previously embedded primary prompt. The new kernel explains the profile lifecycle without changing that compatibility body.

The global store is initialized on the first primary instruction activation, not on server construction. Existing nonblank global `.jcode/system-prompt.md` and `.jcode/prompt-overlay.md` sources are imported into the first managed commit with durable receipts. Originals remain untouched. A valid initialized store is reused. A damaged initialized store blocks activation and requires repository recovery.

Projects without a configured managed store retain compatible project `.jcode/system-prompt.md` and `.jcode/prompt-overlay.md` behavior. Dedicated global and project `AGENTS.md` inputs remain outside managed repositories. Global contributions now precede project contributions. Preferred-tool files remain compatibility inputs until their later Phase 3 migration.

## Complete activation composition

The composer owns one complete operation. It resolves sources, defaults, specificity, agent availability, modules, project redefinitions, addenda, and complete rendering. Callers do not assemble prompt layers.

The current stable slots are:

1. Agent-profile kernel
2. Selected agent
3. Enabled capability resources such as Mermaid
4. Self-development mechanics for self-development sessions
5. Global common guidance
6. Project common guidance or compatible project overlay
7. Dedicated global then project `AGENTS.md`
8. Applicable project agent addenda
9. Global then project preferred-tool compatibility guidance
10. Available-skills catalog snapshot

Memory contributes no active prompt material while `features.memory = false`. Startup Context file snapshots remain authoritative user messages outside the true system prompt.

## Freezing and dispatch

Activation renders complete current sources and persists:

- Exact system-prompt text
- Active agent ID, display name, and scope
- Independent first-provider-dispatch time
- Optional active-transition message ID for WP-04
- Active rendered skill foundation for WP-05

The exact stored static text is used by app-core blocking turns, app-core streaming turns, local TUI request construction, `jcode run`, direct REPL, shared-server sessions, ACP, and Harness-created sessions. Local and remote paths therefore share the same session authority rather than rereading files per request.

Managed source edits, legacy file edits, `AGENTS.md` edits, skill catalog edits, and config reloads do not change an existing session's stored static prompt. Provider preflight and cache signatures account for the complete stored text. Dynamic active-skill text, memory when enabled elsewhere, current-turn reminders, and Swarm effort directives remain late prompt material.

The first dispatch boundary is persisted before opening the provider request. It is independent of Startup Context, including when the Startup Context plan is empty. If dispatch persistence fails, no provider request is opened and the exact prior activation remains.

## Lifecycle

- Resume, reconnect, takeover, reload, and split reuse exact stored text and active identity without instruction source access.
- Clear retains the active identity, renders current sources, recaptures Startup Context, and publishes only after both activations succeed.
- Transfer retains the active identity, renders current sources, clears active-skill state, recaptures Startup Context, appends the existing handoff, and removes a failed unpublished child completely.
- An old session without stored prompt state renders and persists the current `global:jcode` compatibility composition when first restored. This does not claim historical prompt recovery and does not rewrite transcript messages.
- `/agent <selector>` before first provider dispatch replaces provisional system state and appends no message. Re-selecting the same resolved identity is a no-op.
- Post-dispatch ordinary switching and explicit true-system replacement are intentionally unavailable until Phase 3 WP-04.

## Storage and recovery

Frozen prompt and active-skill text are snapshot state, not append-journal metadata. A change to either forces an atomic full session snapshot so a large prompt is not duplicated in every later journal entry. Startup stubs and remote startup loads retain the exact activation metadata they need. Session memory diagnostics account for frozen prompt and active-skill bytes.

Instruction activation errors identify the selected resource or repository operation and leave prior session state unchanged. Fresh-session failures remove unpublished session artifacts and tool policy. Interactive clients receive a visible error. CLI, REPL, ACP, and Harness creation fail before a provider request.

## Verification boundary

Mechanism tests use synthetic resources for selection, specificity, ordering, freezing, persistence, lifecycle, errors, and transport. The compatibility body and approved kernel are not locked by phrase or snapshot tests. One-time migration evidence is recorded under `docs/dev/instruction-inventory/`.
