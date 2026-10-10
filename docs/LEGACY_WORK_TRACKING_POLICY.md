# Legacy work-tracking policy

Missions, initiatives and the upstream command workflows are retained but globally unavailable in this downstream. `features.legacy_work_tracking = false` is an availability boundary, following the [memory](MEMORY_POLICY.md) and [Swarm](SWARM_POLICY.md) policies. Nothing is deleted.

## What is dormant

While the gate is off, these are unavailable:

- **Missions.** The `/mission` and `/goal` commands stay rejected. The local-turn mission reminder is not rendered and the mission store is not read.
- **Initiatives.** The `initiative` tool, `/initiatives` and its legacy alias `/goals`.
- **Command workflows.** `/commit`, `/commit-push` (and `/commit-and-push`), the release commands (`/fast-release`, `/fast-macos-release`, `/remote-release` and the aliases `/cut-release` and `/commit-push-release`), `/test`, `/plan`, and `/improve` with its `plan`, `status`, `stop` and `resume` forms and its loop mode.

The Swarm-dependent workflows (`/review`, `/judge`, `/autoreview`, `/autojudge`, `/refactor`, `/triage`, `/overnight`) stay governed by the [Swarm policy](SWARM_POLICY.md#legacy-dependent-workflows). This gate does not change them.

## The gate

```toml
[features]
legacy_work_tracking = false
```

The default is `false`. `JCODE_LEGACY_WORK_TRACKING_ENABLED` is the global environment override and takes precedence over the file. When diagnosing availability, inspect both the saved configuration and the environment of the hosting process.

## Unavailable surfaces

While the gate is off:

- `initiative` is absent from new registries, provider definitions, `batch` member calls, debug tool listings and suggestions. A previously constructed registry cannot re-expose or execute it. Direct execution rejects before decoding parameters or touching storage. The isolated-child restriction on initiative updates is unchanged and unreachable.
- The command workflows reject in the TUI before rendering, changing a loop mode, appending a turn or requesting an interruption, locally and over a remote connection. The server's `RenderWorkflowPrompt` branch rejects them before reading instruction sources, so the Harness API and SDKs get the same refusal. Pending preparation restored after a reload cannot dispatch them.
- Slash completions, `/help`, `/help <command>` and the help overlay do not offer these commands. Typing one shows the retirement notice and is never sent to the model as a message.
- A saved improve loop mode in a session is ignored on restore. It is not deleted or rewritten.
- The goal store and the mission store fail closed, including loads, listings and session attachments. Goal saves never sync into memory unless memory is itself enabled, so the memory store is never reached while memory is off.

## Retained data

Nothing is deleted or migrated:

- Goals live under `~/.jcode/goals/` (`global/`, `projects/<hash>/` and `sessions/` attachments, with any `.bak` files).
- Missions, if any exist, live under `~/.jcode/missions/`.
- The managed instruction resources (`modules/mission-*` and the `workflow-*` modules and notifications) stay in the instruction stores. The [instruction manager](INSTRUCTION_MANAGER.md) can still inspect them. Reading or editing them does not make a workflow available.
- Historical side-panel goal pages (`goals`, `goal.<id>`) stay in each session's side-panel state and still render. They are not provider context.
- Earlier `initiative` calls and results stay in session transcripts and render as before.

Do not delete or rewrite this data as part of ordinary maintenance. Disposal needs an explicit user decision and its own preservation plan.

## What still works

- SDK structured output, which uses the same `RenderWorkflowPrompt` operation with its own request kinds.
- `/transfer`, until its own retirement.
- Todos, the side panel and ordinary direct requests to commit, test or plan.

## Reactivation

Reactivation needs an explicit product decision, the configuration change (or the environment override), a process restart so tool lists and session state are rebuilt consistently, and focused verification of the dormant enabled paths. Changing a prompt, skill or saved session does not grant availability.

## What replaces them

Session workflows, stop declarations and the workflow guidance written later. Plans and progress live in project documents.

## Cache boundary

A session whose persisted tool set held `initiative` records one withdrawn removal, announced by one appended notice; later requests stay stable ([tool set](CLAUDE_PROVIDER_PARITY.md#tool-set)). On Claude models that take tool changes inside a message, the first-sent `tools` array is unchanged and the notice carries the removal of that declared tool, so the prompt cache and earlier thinking survive. On every other provider the definition leaves the `tools` array at one recorded tool-set transition. Reactivating the gate returns the tool as an addition, the same way in reverse. Earlier transcript content is not rewritten.

## Maintainer verification

Mechanism tests use synthetic content and private homes. They do not judge prompt wording.

```bash
python3 scripts/run_isolated_test.py cargo test --profile selfdev -p jcode-base --lib legacy_work_tracking -- --test-threads=1
python3 scripts/run_isolated_test.py cargo test --profile selfdev -p jcode-app-core --lib legacy_work_tracking -- --test-threads=1
python3 scripts/run_isolated_test.py cargo test --profile selfdev -p jcode-tui --lib legacy_work_tracking -- --test-threads=1
```

The native acceptance drives a placed fixture session on a candidate binary with a private home and a localhost scripted provider: the one announced tool-set change and none afterwards, refused initiative calls (direct and in `batch`), the retired workflow protocol, SDK structured output, transfer, a rollback and a second retirement, and byte-identical goal files:

```bash
python3 scripts/run_isolated_test.py python3 scripts/verify_legacy_work_tracking.py --binary <candidate>
```

Tests of the dormant enabled paths set `JCODE_LEGACY_WORK_TRACKING_ENABLED=true` in their own isolated environment. Never enable the gate on a real installation as a test fixture.
