# Agent profiles and frozen system prompts

Jcode primary sessions use one named agent, one exact stored true system prompt, and an explicit profile-transition lifecycle.

## Initial selection

Selection precedence for a new primary session is:

1. An explicit selector from `jcode --agent <selector>`, remote `Subscribe.agent`, Harness `CreateSession.agent`, or `/agent <selector>` before first dispatch
2. `default_agent` in the configured project instruction store
3. `default_agent` in the global instruction store
4. The effective unqualified `jcode` compatibility agent

Selectors accept `name`, `global:name`, or `project:name`. Unqualified selectors use project-first specificity. A configured or explicit missing, invalid, ambiguous, or primary-unavailable agent fails. Jcode does not continue to a lower selection tier after a configured tier fails.

The Rust SDK exposes `create_session_with_agent`. The TypeScript SDK accepts the optional second `createSession(workingDir, agent)` argument. The Harness bridge advertises `initial_agent_selection`; both SDKs reject an agent-bearing request before sending it when the connected bridge lacks that capability.

## Commands

```text
/agent
/agent <name|global:name|project:name>
/agent replace [name|global:name|project:name]
/agent inspect
```

- `/agent` opens the valid primary-agent picker for the active project.
- Before first provider dispatch, `/agent <selector>` replaces the provisional complete true-system composition. It appends no transcript message, makes no provider call, and creates no cache event.
- After first provider dispatch, `/agent <selector>` appends a complete current-source target profile as a Jcode-generated hidden user-authority message. It makes no provider call and preserves the existing system prefix.
- `/agent replace` opens the replacement picker. `/agent replace <selector>` explicitly installs the latest complete composition as the true system prompt.
- `/agent inspect` displays the exact current stored system prompt, active agent delivery, and active rendered skill text.
- Agent changes are idle-only. Busy sessions reject them rather than queueing them.
- Selecting the exact current qualified identity ordinarily does nothing and does not reread source. An unqualified selector still resolves current project-first specificity before Jcode classifies it as unchanged.

The former special-role model picker is `/agent-models`. `/agents` remains a compatibility alias and displays guidance to use `/agent` for primary profiles.

## Compatibility agent and kernel

The shipped global instruction seed contains:

- `system/kernel.md`
- `system/common.md`
- `system/mermaid.md`
- `agents/jcode.md`
- `notifications/agent-transition.md`
- `notifications/agent-replacement.md`

`agents/jcode.md` is the compatibility profile. Its rendered body is byte-identical to the previously embedded primary prompt. The profile kernel explains append transitions without changing that compatibility body. Transition and replacement sentences are managed resources. Their wrapper, message role, structural identity, cache behavior, and persistence remain code-owned.

Existing initialized stores receive newly shipped seed resources through a versioned, automatic local commit. The upgrade adds only paths introduced by the new seed, never overwrites an existing working file, and never recreates a resource the user deleted after adopting that seed version.

## Complete activation composition

The composer owns one complete operation. It resolves sources, defaults, specificity, agent availability, modules, project redefinitions, addenda, and complete rendering. Applicable invalid or ambiguous addenda fail activation; unrelated damaged addenda remain isolated.

The stable true-system slots are:

1. Agent-profile kernel
2. Selected agent
3. Enabled capability resources
4. Self-development mechanics for self-development sessions
5. Global common guidance
6. Project common guidance or compatible project overlay
7. Dedicated global then project `AGENTS.md`
8. Applicable project agent addenda
9. Global then project preferred-tool compatibility guidance
10. Available-skills catalog snapshot

An ordinary post-dispatch transition renders only the complete target agent, its modules, applicable project redefinition, and applicable project addenda. It does not duplicate the kernel or independent system sections.

Memory contributes no active prompt material while `features.memory = false`. Startup Context file snapshots remain authoritative user messages outside the true system prompt.

## Ordinary post-dispatch transition

Jcode stores the complete profile in `Session.messages` as a user-role message with system display role and no provider-visible timestamp. The message uses the code-owned `<jcode_agent_profile>` wrapper and a managed transition sentence.

The same atomic session snapshot updates:

- Active agent identity
- Active transition message ID
- Complete authoritative message content
- Structural `agent_profile_message_ids` identity index

A prior transition becomes superseded when the active ID changes. Superseded profile messages remain structurally recognizable cards but become ordinary historical context.

The TUI shows an appended profile as a compact collapsed card. Expanding the card reveals the complete stored text. Wide and narrow layouts use the same state and content.

An ordinary transition does not change provider, model, effort, tool definitions, Startup Context, memory policy, active skill, todos, side panels, or provider-native continuation. The next request sees legitimate append-only message growth.

## Explicit true-system replacement

`/agent replace` renders the complete latest composition and stages it against the current session. Before persistence Jcode validates:

- Selected agent and complete composition
- Current provider projection
- Complete request accounting with the replacement static prompt, current dynamic instructions, and current tool definitions

A blocked replacement leaves the old session unchanged.

A successful replacement atomically persists:

- New complete true system prompt
- New active agent identity
- Cleared active-transition protection
- Cleared stored provider-native continuation
- A short managed system-display audit message

After persistence Jcode resets in-memory provider continuation and prompt-cache tracking, records the intentional invalidation cause, and reseeds context accounting. Tool-definition locking remains independent and is not unlocked by replacement. No provider call occurs until a later user turn.

Same-agent replacement is allowed. It is the explicit way to load current source into the true system prompt.

## Context Editor and rewind

The active appended profile is a protected authoritative message:

- The Context Editor marks it as locked and keeps complete block detail inspectable.
- Summary range preview and curator capture reject a range covering it before any curator model call.
- Unrelated messages and superseded profile messages retain ordinary context-control behavior.
- Context apply, revert, reapply, projection validation, and curator prompts are otherwise unchanged.

Rewind changes conversation history, not current agent configuration. If rewind would truncate the active appended profile, Jcode pins that exact stored message at the new tail boundary. Undo restores the exact prior transcript and structural profile identity without duplicating the active message. Existing provider-continuation reset and context-transaction reconciliation still apply.

## Lifecycle

- Resume, reconnect, takeover, process reload, and split retain exact system text, active identity, profile-message identities, active transition, and active skill state without instruction source access.
- Clear and transfer retain the active identity but render current source for their new true system context. They begin with no appended active profile.
- Model and provider changes retain exact prompt and profile state.
- A true-system replacement clears active-message protection but does not rewrite or delete earlier transition messages or context transactions.
- Persistence failure leaves the complete previous session state active.

## Inspection, exports, and replay

- Raw session export contains complete current system prompt, active skill, structural profile identities, and complete profile messages.
- Markdown export begins with complete `System prompt` and `Active skill` sections and labels appended transitions as `Agent Profile`.
- Profile content and current instruction state are not omitted by privacy redaction. This is an explicit instruction-inspection/export path.
- Replay timelines include one exact `session_instructions` event plus complete profile-transition display events. Interactive replay starts from the retained exported Session and renders profile cards without rereading instruction repositories.
- Ordinary remote History does not include the separate current true system prompt. `/agent inspect` is the explicit on-demand route for complete current system and skill text.

## Remote, Harness, and SDK interfaces

The internal protocol supports `SetAgent`, `GetAgentCatalog`, and `GetAgentStatus`. Agent selection events identify provisional, unchanged, appended, and replaced outcomes.

The Harness bridge advertises `agent_profile_controls` and exposes:

- `list_agents` / `listAgents`
- `set_agent` / `setAgent`
- `inspect_agent` / `inspectAgent`

Rust and TypeScript SDKs fail before sending a profile-control request to an older bridge that does not advertise the capability.

## Storage and recovery

Frozen prompt and active-skill text remain full snapshot state. Profile transitions force an atomic full session snapshot so prompt bodies and structural IDs cannot diverge. Metadata-only startup stubs retain only active agent, dispatch, transition, and skill identity; they do not load complete prompt or skill bodies. Full restore, remote startup history, replay, and export retain exact content where required.

Instruction activation errors identify the selected resource or repository operation and leave prior session state unchanged. Fresh-session failures remove unpublished session artifacts and tool policy.

## Verification boundary

Mechanism tests use synthetic resources for selection, specificity, append composition, replacement composition, store upgrades, persistence, cache transitions, context locking, rewind pinning, export, replay, protocol, Harness, SDK, and UI behavior. The approved compatibility, kernel, transition, and replacement prose is not locked by phrase or snapshot tests.
