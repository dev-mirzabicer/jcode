# jcode Docs

Reference documentation for the jcode codebase.

## Layout

- `docs/*.md` — architecture, feature, and behavior docs (current state of the system).
- `docs/plans/` — forward-looking plans, roadmaps, and TODO trackers. May be partially implemented or stale.
- `docs/audits/` — point-in-time audits and reviews. Historical snapshots, not kept up to date.
- `docs/proposals/` — design proposals not yet committed to.
- `docs/dev/` — developer-facing process and testing notes.

## Key entry points

- Architecture: `SERVER_ARCHITECTURE.md`, `MODULAR_ARCHITECTURE_RFC.md`, `CRATE_OWNERSHIP_BOUNDARIES.md`
- Swarm: `SWARM_ARCHITECTURE.md`, `SWARM_TASK_GRAPH.md`
- Agent profiles and prompt freezing: `AGENT_PROFILES.md`, `SYSTEM_PROMPT_CONFIG.md`; managed Git stores: `INSTRUCTION_STORES.md`; skills: `SKILLS.md`
- Agent memory policy: `MEMORY_POLICY.md`; dormant implementation: `MEMORY_ARCHITECTURE.md`
- Process RAM and allocator diagnostics: `MEMORY_BUDGET.md`, `MEMORY_INCIDENT_RUNBOOK.md`
- Startup Context: `STARTUP_CONTEXT.md`, `dev/STARTUP_CONTEXT_ACCEPTANCE.md`
- Context control: `CONTEXT_CONTROL.md`, `dev/CONTEXT_CONTROL_ACCEPTANCE.md`
- Refactoring and quality: `REFACTORING.md`, `plans/CODE_QUALITY_10_10_PLAN.md`
- Desktop app: `DESKTOP_APP_ARCHITECTURE.md`, `DESKTOP_CODEBASE_ARCHITECTURE.md`
- Providers: `PROVIDER_DOCTOR.md`, `AWS_BEDROCK_PROVIDER.md`
- Platform: `WINDOWS.md`, `TERMINAL_CAPABILITIES.md`

## Conventions

- Docs describing current behavior live at the top level; anything speculative goes in `plans/` or `proposals/`.
- Prefer updating an existing doc over adding a near-duplicate.
- Root of the repo should only hold README, CONTRIBUTING, RELEASING, AGENTS, LICENSE, and similar meta files. Put everything else here.

## Managed notification and control prose

See [managed notification and control prose](NOTIFICATIONS.md) for current-source occurrence rendering, structural ownership, typed todo queues, failures and recovery.
