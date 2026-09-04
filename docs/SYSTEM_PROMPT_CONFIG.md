# Configuring primary agent instructions

Jcode composes one complete static system prompt when a primary session activates and stores the exact rendered text in that session. See [`AGENT_PROFILES.md`](AGENT_PROFILES.md) for selection, lifecycle, persistence, and recovery.

## Managed stores

Global managed instructions live in:

```text
~/.jcode/instructions/
```

Configured project stores use the repository modes in [`INSTRUCTION_STORES.md`](INSTRUCTION_STORES.md). The working tree is runtime authority. The initial global seed contains the profile kernel, an empty common layer, Mermaid guidance, and the `jcode` compatibility agent.

A global store is initialized on the first primary activation. Server construction alone performs no instruction repository I/O.

## Initial agent selection

Use any of:

```text
jcode --agent reviewer
jcode --agent global:jcode run "message"
jcode --agent project:reviewer repl
```

In a fresh TUI session before the first provider request:

```text
/agent reviewer
/agent global:jcode
/agent project:reviewer
```

Remote `Subscribe` and Harness `CreateSession` requests also accept an optional `agent` selector. Rust SDK callers can use `create_session_with_agent`; TypeScript callers can use `createSession(workingDir, agent)`. The Harness bridge advertises `initial_agent_selection`, and both SDKs fail before sending an agent-bearing request to an older bridge that lacks it. Ordinary creation without an agent remains compatible.

Selection precedence is explicit selector, project-store `default_agent`, global-store `default_agent`, then effective `jcode` compatibility fallback. An invalid configured tier fails instead of falling through.

## Static composition order

The current stable slots are:

1. Agent-profile kernel
2. Selected agent
3. Enabled capability resources
4. Self-development mechanics for self-development sessions
5. Global common guidance
6. Project common guidance or compatible project overlay
7. Global then project `AGENTS.md`
8. Applicable project agent addenda
9. Global then project preferred-tool compatibility guidance
10. Available-skills catalog snapshot

Memory is absent while globally disabled. Startup Context file snapshots remain authoritative user messages and are not system-prompt layers.

## Legacy compatibility

On first global-store initialization:

- A nonblank `~/.jcode/system-prompt.md` is imported as the initial managed `global:jcode` body.
- A present `~/.jcode/prompt-overlay.md` is imported as global common guidance.
- Originals remain unchanged and become inactive only because durable import receipts prove the managed equivalent.

A project legacy system prompt or overlay remains a compatibility input only while the corresponding managed project resource is genuinely absent and no import receipt has completed cutover. A present invalid or ambiguous managed resource fails; it cannot disappear behind legacy fallback. Explicit `global:` selection remains global even when project legacy input exists. A configured project store can define or redefine agents and common guidance. Present valid legacy and managed definitions with no matching import receipt fail rather than contributing twice.

`AGENTS.md` remains a dedicated ecosystem input and is not imported automatically. Preferred-tool files remain live compatibility sources until their later managed migration.

## Session freezing

After activation, edits to managed resources, legacy prompt files, `AGENTS.md`, preferred-tool files, or the available-skill catalog do not change that session's static prompt. Config reload also does not replace stored static text.

Resume, reconnect, takeover, reload, and split reuse exact stored text. Server-backed and direct clear retain the active agent identity, render current sources, recapture Startup Context, and install the new context only after complete persistence succeeds. Transfer follows the same fresh-source rule and clears active skill. Old sessions without stored prompt state stage and durably persist the current `global:jcode` compatibility composition before replacing live Agent state; failed migration leaves the previously active Agent unchanged, and successful migration clears provider-native continuation before another request.

Applicable invalid or ambiguous project addenda fail composition. Invalid unrelated addenda remain isolated. Unqualified pre-dispatch selection resolves project-first specificity before Jcode decides whether the operation is a same-agent no-op.

Post-dispatch ordinary profile switching and explicit true-system replacement arrive in Phase 3 WP-04. `/agent` therefore rejects a session whose first provider dispatch has already occurred.

## Dynamic prompt material

The following remain late request material rather than part of the stored static prompt:

- Active skill text until WP-05 adopts its persisted activation state
- Current-turn system reminders
- Swarm effort directives
- Memory only in runtimes where memory is enabled; Mirza's downstream globally disables it

Tool definitions retain their independent lock and one intentional late-MCP rebuild behavior.
