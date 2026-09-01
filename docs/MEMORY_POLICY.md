# Memory policy

Jcode's agent-memory implementation is retained but globally disabled in Mirza's downstream harness.

## Current behavior

The effective default is:

```toml
[features]
memory = false
```

This setting is a global availability boundary. While it is false:

- Jcode does not retrieve memories for a turn.
- No memory content enters the dynamic system prompt or message suffix.
- Jcode does not extract memories from sessions during turns, save, shutdown, disconnect, stop, crash, or reload handling.
- The memory agent and sidecar do not start through production session paths.
- The agent does not receive a `memory` tool definition.
- Direct or nested memory tool execution is rejected.
- `/memory on` cannot override the global setting.
- `jcode memory ...` commands reject before accessing memory storage.
- Ambient execution does not read memory graphs, inject memory feedback, advertise memory gardening, or run post-cycle memory maintenance.
- The memory activity widget and memory model picker are hidden.

`JCODE_MEMORY_ENABLED=true` remains a global environment override. Changing the global policy requires a process restart so the provider tool list and session state are rebuilt consistently.

## Stored data

Disabling memory does not delete or migrate existing data. Memory graphs and backups remain under:

```text
~/.jcode/memory/
```

Historical session memory-injection records also remain as historical display and replay evidence. They are not automatically reused as new provider context.

Do not delete or rewrite this data as part of ordinary maintenance. Disposal or reactivation requires an explicit user decision and its own preservation or migration plan.

## Separate context sources

The following are not agent memory and remain available:

- Startup Context, which contains files explicitly selected by the user
- `AGENTS.md`, prompt overlays, and preferred-tool guidance
- Active skills
- Current-turn system reminders
- Explicit session search
- Context Editor projection, summaries, reasoning suppression, and tool-result distillation

Startup Context and Context Editor behavior remain authoritative under their own documentation. Memory disablement does not alter their messages, receipts, projections, or cache ordering.

## Cache behavior

Removing the memory tool causes one intentional provider-prefix transition when the disabled policy is activated. After restart, the tool list remains stable without memory and no turn-specific memory suffix is added.

Jcode does not rewrite prior transcript content to announce the policy change.

## Retained implementation

The implementation remains in source for history, comparison, and a possible future explicit reactivation. The detailed dormant architecture is documented in [`MEMORY_ARCHITECTURE.md`](MEMORY_ARCHITECTURE.md).

The similarly named [`MEMORY_BUDGET.md`](MEMORY_BUDGET.md) and [`MEMORY_INCIDENT_RUNBOOK.md`](MEMORY_INCIDENT_RUNBOOK.md) describe process RAM and allocator diagnostics. They are active and are not disabled by this policy.

## Troubleshooting

Use:

```text
/memory status
```

A disabled installation reports that global configuration owns the state. A session-level enable attempt fails without changing the session.

If memory appears in a new provider request while the global setting is false:

1. Check `JCODE_MEMORY_ENABLED`.
2. Confirm the process was restarted after the configuration change.
3. Inspect the active tool list and runtime identity.
4. Preserve `~/.jcode/memory` unchanged.
5. Treat the behavior as a defect in the global gate rather than deleting data or editing session history.
