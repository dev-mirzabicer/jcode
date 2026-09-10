# Swarm availability policy

Swarm is globally unavailable by default in this downstream. `features.swarm = false` is an availability boundary, not merely the initial value of a session toggle. The retained implementation, instruction assets and historical data are not deleted.

## Configuration and reactivation

```toml
[features]
swarm = false
```

`JCODE_SWARM_ENABLED` remains the existing global environment override and takes precedence over the file. Inspect both the actual serialized configuration and the hosting process environment when diagnosing availability. A source-default change does not override a previously saved `true` value.

`/swarm status` reports unavailability. `/swarm on`, protocol enable requests, and Swarm effort selection cannot exceed the global setting. Ordinary sessions cannot reactivate it. A deliberate future reactivation requires an explicit product decision, global configuration change, process restart, and focused verification of the dormant enabled paths. Changing a routing prompt, model role, skill or saved session does not grant availability.

## Unavailable surfaces

While globally disabled:

- `swarm` is absent from new registries, names, provider definitions, debug tool listings and suggestions. A previously constructed registry cannot re-expose or execute it.
- The `communicate` compatibility alias and batch member calls receive the same global rejection. The existing prohibition on batching `batch` itself remains unchanged.
- Direct tool execution rejects before decoding action parameters, transport connection, hooks or coordination effects.
- Every `Comm*` coordination request rejects at both first-request and attached-client dispatch. Global enable and effort rejection precede session allocation or busy-control handling where applicable.
- Swarm debug commands, planner/worker execution, mutation replay and background-await restoration cannot launch model work.
- The Swarm routing editor and special-model role are absent from active discovery. Direct or stale UI actions cannot edit the dormant source or saved role through those workflows.
- `swarm` and `swarm-deep` are not offered as effort choices. Their dynamic instructions do not render, including when an old session retains a historical sentinel.

Historical effort metadata is not rewritten. Retained provider adapters can still decode old sentinels to their maximum supported reasoning effort, but that mapping does not enable coordination or inject Swarm instructions.

## Legacy dependent workflows

The legacy `/review`, `/judge`, `/autoreview`, `/autojudge`, `/refactor` (including plan/resume), `/triage`, and `/overnight` execution workflows are retired under the same global availability boundary. Their contracts required or recommended Swarm or its `communicate` alias. No alternative delegation system or rewritten role prose replaces them here.

Launch/discovery, automatic triggers, typed rendering/splitting and delayed dispatch reject before source reads or child publication. Saved automatic-review/judge flags remain historical metadata, but cannot make those runtimes effective. Retained refactor mode cannot trigger automatic todo followups. Ordinary `/plan`, `/improve`, generic notifications/scheduling, primary split/transfer, structured output, and direct user requests to review or refactor code remain separate.

Explicit status/off controls remain meaningful without launching a model. Overnight status, logs, historical review-page inspection and cancellation remain available on explicit request, although they are not advertised as an active workflow.

An old review startup receipt, or an input receipt containing the existing code-marked overnight followup, is not automatically replayed. The complete receipt is moved beside its original path with a unique `.retired-swarm-*` suffix, and the TUI identifies that recovery location. This preserves mixed unsent input and original instructions without injecting them. Move failures are reported rather than claimed as successful preservation. Typed interrupted command preparation remains suspended and its execution policy is rechecked before dispatch. Historical conversation prose is not scanned or rewritten to enforce retirement.

## Dormant data and instruction preservation

Loading Swarm state previously also migrated legacy files and pruned old records. Global disablement returns before those storage operations. Snapshot version reads, persistence, deletion, mutation-state replay and await-state discovery are also unavailable. Expired files and backups remain intact rather than being silently cleaned up.

The relevant retained stores include durable `state/swarm` snapshots and the legacy runtime Swarm, await-members and mutation directories. No migration of historical Swarm output into a replacement delegation store is implied.

Exact captured `Session.swarm_routing_prompt`, old effort fields, conversation messages and historical render/export content remain historical source. Disabling an active tool removes its provider-facing contribution, not its old transcript evidence. The central instruction manager may still inspect retained resources independently of runtime activation.

## Shared infrastructure that remains active

Names are not ownership boundaries. `SwarmMember` and related maps also carry ordinary session presence, event senders and runtime status. Their non-coordination responsibilities remain:

- Session registration, attached-client event fanout and ordinary notifications.
- Background task progress, completion, notify/wake and ordinary cancellation.
- Generic headless sessions and scheduling infrastructure.
- Self-development build, test, reload and canary controls.
- Terminal-presence collection and power inhibition for active ordinary sessions.

Swarm grouping, coordinator messages, plan broadcasts, task staleness work and Swarm worker reaping remain gated. This policy does not implement the replacement isolated delegation system or change shared cancellation/output semantics.

## Cache boundary

Removing Swarm is an intentional provider-prefix transition. An existing Agent tool lock drops the unavailable definition once and invalidates its continuation. Subsequent requests remain stable without Swarm. New registries and restored sessions build their available definitions under the same global policy. Frozen historical routing text is neither reapplied nor cleared, and old conversation messages are not rewritten.

## Maintainer verification

Focused mechanism tests use synthetic instruction content and providers that record or reject model execution. They check the public Registry, direct aliases, batches, real server stream pairs, storage owners, cached definitions, and physical local/remote TUI input. They do not judge prompt wording.

```bash
cargo test --profile selfdev -p jcode-base -p jcode-app-core --lib swarm_retirement -- --test-threads=1
cargo test --profile selfdev -p jcode-tui --lib swarm_retirement -- --test-threads=1
```

Run through coordinated `selfdev test` when developing Jcode. Dormant enabled-path suites require explicit isolated configuration. Never enable real Swarm as a test fixture. Before and after activation, inspect protected state, verify the installed current/shared binary identities and canary, and exercise the activated unavailable surfaces without model work.

The [retained architecture](SWARM_ARCHITECTURE.md) and [task-DAG design](SWARM_TASK_GRAPH.md) describe dormant implementation, not available downstream workflows. [Managed workflow instructions](WORKFLOW_INSTRUCTIONS.md) documents source ownership separately from execution availability.
