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
- Swarm availability: [hard-disable policy](SWARM_POLICY.md); retained dormant implementation: `SWARM_ARCHITECTURE.md`, `SWARM_TASK_GRAPH.md`
- Instructions: [overview](INSTRUCTIONS.md), [migration and recovery](INSTRUCTION_MIGRATION.md), [inspection and editing manager](INSTRUCTION_MANAGER.md)
- Instruction framework verification: [independent phase closeout](dev/INSTRUCTION_CLOSEOUT_ACCEPTANCE.md)
- Agent profiles and prompt freezing: `AGENT_PROFILES.md`, `SYSTEM_PROMPT_CONFIG.md`; managed Git stores: `INSTRUCTION_STORES.md`; skills: `SKILLS.md`; launch-time model policy: `MODEL_ROSTER.md`; maintainer ownership and future adoption: `dev/INSTRUCTION_RUNTIME.md`; combined acceptance: `dev/INSTRUCTION_INTEGRATION_ACCEPTANCE.md`
- Agent memory policy: `MEMORY_POLICY.md`; dormant implementation: `MEMORY_ARCHITECTURE.md`
- Process RAM and allocator diagnostics: `MEMORY_BUDGET.md`, `MEMORY_INCIDENT_RUNBOOK.md`
- Tool execution: [retained output, exact reads and cancellation](TOOL_EXECUTION.md), [storage/control architecture](dev/EXECUTION_STORAGE.md), [producer inventory](dev/EXECUTION_PRODUCERS.md), [acceptance reconciliation](dev/EXECUTION_ACCEPTANCE.md)
- Primary runtime: [creation, detached ownership and native verification](dev/PRIMARY_HOST_ACCEPTANCE.md), [durable input and atomic location controls](PRIMARY_INPUT_LOCATION.md), and [reviewed new-context scope](PRIMARY_CONTEXT_SCOPE.md). Managed creation and location controls are explicitly staged; ordinary client detach does not stop hosted work.
- Workspace foundation: [private catalog and recovery](dev/WORKSPACE_CATALOG.md), [independent checkout provisioning, adoption and path-only Startup Context copy](dev/WORKSPACE_CHECKOUTS.md). Managed session/scope rollout and the human management client remain separately gated.
- Task monitor: [native task/output controls, child Context Editor and storage review](TASK_MONITOR.md)
- Runtime control: [reviewed CLI shutdown, explicit Start and offline receipts](RUNTIME_CONTROL.md), [ownership and failure contract](dev/RUNTIME_SHUTDOWN.md)
- Combined execution/delegation verification: [requirements, native journeys and evidence boundaries](dev/PHASE4_INTEGRATION_ACCEPTANCE.md)
- Independent Phase 4 closeout: [requirement reconciliation, repairs and acceptance limits](dev/PHASE4_CLOSEOUT_ACCEPTANCE.md)
- Isolated children: [workflow, permissions, MCP eligibility and recovery](ISOLATED_DELEGATION.md)
- Session inspection: [snapshots, activity, archival and reviewed cleanup](SESSION_INSPECTION.md), [persistence and ownership](dev/SESSION_INSPECTION.md), [acceptance evidence and limits](dev/SESSION_INSPECTION_ACCEPTANCE.md)
- Startup Context: `STARTUP_CONTEXT.md`, `dev/STARTUP_CONTEXT_ACCEPTANCE.md`
- Context control: `CONTEXT_CONTROL.md`, `dev/CONTEXT_CONTROL_ACCEPTANCE.md`
- Claude provider parity (INT-01): [what a Claude request contains, thinking replay, tool-set lifetime, cache placement and per-model parameters](CLAUDE_PROVIDER_PARITY.md); [acceptance ledger, live provider contract and `provider-doctor --contract claude-oauth`](dev/CLAUDE_OAUTH_PARITY_ACCEPTANCE.md)
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

See [managed workflow and specialist instructions](WORKFLOW_INSTRUCTIONS.md) for command preparation, transfer/review/overnight/Swarm consumers, source cutover, routing snapshots and failure recovery.
