# Isolated delegation verification

Status: implementation verification record. This is not work-package acceptance.

## Production owners

- `jcode-app-core::delegation`: complete host create/send, warm runtime reuse and
  dedicated standalone relay. Normal shared clients call the host directly.
- `jcode-base::execution`: invocation input, atomic child FIFO/admission,
  cancellation cohorts, retained output, activity and lost-owner recovery.
- `Session`: fixed child origin/profile/model/workspace/artifact identity, current
  permission/preset notices, frozen instructions and authoritative history.
- Instruction composer and model roster: existing source selection/rendering and
  independent concrete provider construction. No second renderer or roster.
- Registry and MCP manager: immutable per-turn native permissions, configured MCP
  eligibility, exact shared launch identity, child-owned connections and cleanup.
- Existing context transaction service: range locks, stale-draft checks, rewind and
  projection. The task-monitor navigation remains a separate UI integration.

## Requirement mapping

| Contract | Concrete checks |
|---|---|
| Explicit profile/alias/permission, invalid source and no orphan | `instruction::composition::isolated::tests`, `delegation::tests::hosted_rejections_publish_no_child_and_continuation_does_not_transfer_control`, tool-types normalization tests |
| Independent model/effort, exact source-free restore | Existing `model_roster::` factory/resolver tests, `ModelRosterResolution::validate_provider`, native hosted artifact/follow-up test and actual process-restart fixture |
| Normal system plus child addition, preset in user history | Synthetic composer prefix/preset tests, captured native HTTP payloads, child Session transition tests and managed guidance freeze tests |
| Latest/custom/empty/disabled Startup Context | `hosted_startup_capture_uses_latest_custom_and_disabled_without_mutating_default`; existing Startup Context capture/provider-budget and preservation family |
| Native mutation scope | `child_mutator_matrix_checks_every_target_before_effects`, artifact-policy symlink/traversal tests and actual native artifact write |
| Immutable background permissions and no child administration | `background_registry_keeps_original_permission_after_turn_change_and_cleanup`, initiative negatives, provider-route rejection and original-parent control tests |
| MCP eligibility and ordinary stateful follow-up | `mcp::access::tests`, pool/manager tests, `ordinary_child_followups_reuse_stateful_mcp_until_runtime_eviction`; classification defaults false and blocklist excludes exact names |
| Original-parent ownership, no direct child chat | Child Session roundtrips, split read-versus-control tests, direct primary-restore negative and actual daemon resume |
| Foreground/background identity and no replay | Native Registry/host tests, immutable acceptance receipt tests, exact transport-replay counter and JSON/NDJSON/REPL workflows |
| FIFO settings and targeted cancellation | `execution::delegation::tests`, `hosted_fifo_capacity_settings_and_stop_preserve_original_inputs`; queued content remains in original invocation records |
| Stop/reload/crash | Synthetic quiet-stream Stop, reload quiescence test, foreground-work recovery gate, actual child process-loss/queued cancellation fixture |
| Fifteen-child admission | Real-store concurrent twenty-start test admits exactly fifteen; native hosted capacity rejection and idle-history reuse |
| Current child directive protection | Child Context Editor snapshot/preview tests, existing context transaction family, rewind/undo checks and render-only native child-lock fixture |
| Compatibility and caller integration | Dedicated host version/namespace negative, Harness/Rust SDK tests, TypeScript SDK native workflow, portable DTO contracts and route-owned native exclusions |
| Prose boundary | Reviewed framework seed resources, synthetic mechanism text only. No wording snapshots, keyword-quality tests or model benchmark |

The mappings above are not aggregate test-count claims. Logs and native fixture
results record the source/runtime boundary, actual outcomes and failed attempts.

## Repeatable commands

All test executables require private state, including HOME fallbacks. Use the
coordinated selfdev test path or the repository wrapper, never bare test binaries
against the live home. See [test isolation](../../scripts/TEST_STATE_ISOLATION.md).

```sh
CARGO_INCREMENTAL=0 bash scripts/dev_cargo.sh test --profile selfdev -p jcode-base --lib instruction::composition -- --test-threads=1
CARGO_INCREMENTAL=0 bash scripts/dev_cargo.sh test --profile selfdev -p jcode-base --lib execution::delegation::tests -- --test-threads=1
CARGO_INCREMENTAL=0 bash scripts/dev_cargo.sh test --profile selfdev -p jcode-base --lib execution::recovery::tests -- --test-threads=1
CARGO_INCREMENTAL=0 bash scripts/dev_cargo.sh test --profile selfdev -p jcode-base --lib mcp:: -- --test-threads=1
CARGO_INCREMENTAL=0 bash scripts/dev_cargo.sh test --profile selfdev -p jcode-base --lib model_roster:: -- --test-threads=1
CARGO_INCREMENTAL=0 bash scripts/dev_cargo.sh test --profile selfdev -p jcode-app-core --lib delegation -- --test-threads=1
CARGO_INCREMENTAL=0 bash scripts/dev_cargo.sh test --profile selfdev -p jcode-app-core --lib agent::isolated::tests -- --test-threads=1
CARGO_INCREMENTAL=0 bash scripts/dev_cargo.sh test --profile selfdev -p jcode-app-core --lib context:: -- --test-threads=1
```

Build the TypeScript SDK using its existing build command before native SDK
acceptance. The macOS native fixture uses a private candidate binary, scripted
localhost inference, a private daemon/API bridge and owned cleanup. It exercises
real transport and provider factories without hosted model calls:

```sh
python3 scripts/run_isolated_test.py python3 scripts/verify_isolated_delegation.py \
  --binary /absolute/path/to/candidate/jcode \
  --evidence-parent /absolute/path/to/owned/evidence
```

The script retains commands, request payloads, events and results. It must report
an overall successful exit, not merely print a partial success dictionary. A
fixture failure does not authorize weakening assertions or treating unrun later
checks as passed.

For visual checks, the private native tester supports
`context-editor-fixture:active-child-directive`. This is render-only synthetic
state. Exercise locked-row selection and capture wide/narrow frames, not arbitrary
mutation of its synthetic Session ID.

## Evidence limits

Native acceptance targets macOS arm64 and the tested local provider/transport
routes. Recording providers and request builders establish mechanics, not hosted
cache billing or prompt quality. Read-only policy is narrow native enforcement
plus configured MCP eligibility and instructions, not an OS sandbox. External
service compensation and vendor-specific cancellation acknowledgment are not
promised. Existing failed broad-suite inventories remain separately documented in
`EXECUTION_FAILURE_TRIAGE.md`; focused passing tests do not rewrite those results.

Final activation must identify the actual running/current/shared binary and its
canary, not merely compilation or an older canary entry. Never activate an
unfinished candidate or downgrade live stores to satisfy an older runtime.

## Observed candidate verification, 2026-09-14

The combined matrix passed: 9 delegation checks, 8 isolated-Agent checks,
138 context/Startup Context checks (one separately identified ignored case),
62 MCP checks and 8 model-roster checks. Separate focused checks covered the
oversized child-context rejection, seven execution-recovery cases, immutable
turn permissions, stateful MCP continuity, and local-versus-remote failure
receipt ownership. Counts overlap and are not presented as unique totals.

Harness API/server/Rust SDK groups passed 16/72/10 checks, TypeScript passed all
50 tests, and changed portable tool/Harness contracts compiled for Linux arm64
and Windows x64. Native runtime parity outside macOS is not claimed. Strict
root/core/TUI/protocol/Harness library/test lint passed at the respective changed
boundaries, along with whole-workspace formatting and diff checks.

The final private actual-binary workflow used 46 scripted localhost responses.
It passed native child-lock frames at 80x24 and 60x24, a waiting daemon parent
with child inspection/artifact creation, exact invocation replay, standalone
JSON/NDJSON, REPL, TypeScript through the actual Harness bridge, active-child
process-loss with cancelled queued intent, host autostart without arbitrary
parent exports, and exact source-free child restore after alias/profile edits.

Failed attempts remain evidence, not passing counts: an invalid fixture runtime
ID, missing private socket directory, wrong root test target, premature native
host-restart timing and unsupported tester-command forwarding were corrected.
Actual native checks also exposed missing overlay frame recording and remote
SIGTERM/panic Session writes. The former now records terminal cells at the existing
Context Editor render boundary; the latter excludes remote processes from
failure-receipt persistence and requires local process ownership. These changes
preserve the existing context and Session authorities.

The earlier verification incident involved tests inheriting live state while an
older daemon ran. Permanent test/bench wrappers isolate HOME, Jcode state, runtime,
XDG and endpoint overrides. Remote quit and remote failure handlers cannot save
Session shadows. A later exact schema comparison confirmed that the recovered
live schema 20 matches the complete candidate schema without downgrade. Live
configuration/instruction sentinels remained unchanged during subsequent private
tests. Recovery and failed-attempt artifacts remain retained separately.
