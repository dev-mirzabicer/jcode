# Integrated instruction acceptance

**Status:** Candidate evidence prepared for Mirza review. This is not package or phase acceptance.

This record reconciles the instruction framework across the runtime, repositories,
primary callers, active session state, manager, model policy and protected
prerequisites. Earlier package counts are supporting evidence, not substitutes
for the combined checks below. Final observations and runtime identity must be
filled before candidate completion.

## Reproduction

Run the combined Rust matrix through coordinated self-development testing:

```text
selfdev test command="bash scripts/test_instruction_integration.sh <artifact-directory>"
```

The runner creates private state and independent logs plus `results.tsv`. It
continues after ordinary failures so later checks are not silently omitted,
returns nonzero if any check fails, and stops on reported memory/disk exhaustion.
A zero-test filter is not acceptance. Read each failed aggregate and compare
relevant source/fresh-process results instead of weakening valid assertions.

After all reload-state-mutating tests, build/reload the TUI and run the activated
production probes with its exact binary:

```sh
python3 scripts/test_instruction_lifecycle.py --binary <binary> --artifact-dir <artifacts>
python3 scripts/test_instruction_callers.py --binary <binary> --artifact-dir <artifacts>
python3 scripts/test_instruction_manager.py --binary <binary> --artifact-dir <artifacts>
python3 scripts/test_instruction_manager_mutations.py --binary <binary> --artifact-dir <artifacts>
python3 scripts/test_instruction_manager_ux.py --binary <binary> --artifact-dir <artifacts>
```

These probes use private homes, temporary projects, real Git, the real daemon,
public protocol/CLI boundaries and PTYs. Any inference requests terminate at
localhost recording fixtures. They do not mutate a real project/store, use paid
models, grade prompt quality or silently accept a partial document.

Also run TypeScript `npm run check` without installing/publishing packages,
provider split/prefix/continuation tests, root production roster tests and the
context-window matrix. Human review covers the current managed central text and
combined UX, not deterministic keyword or prompt-length assertions.

## Requirement map

| Requirement | Production owner and concrete check |
|---|---|
| R-01 inventory | `INSTRUCTION_INVENTORY.md`, all 129 rows, their final dispositions and equality/exception references, plus manual delivery-sink reconciliation |
| R-02 one runtime | `InstructionRuntime::{discover,render,render_registered}`; complete `instruction::tests` and composer families |
| R-03 resource model | `every_resource_kind_parses_and_semantically_round_trips`, metadata kind validation, managed skill and roster distinction |
| R-04 complete render | `deep_finite_graph_and_large_source_render_without_product_caps`; cycles, missing variables/dependencies and restricted helpers fail without partial output |
| R-05 specificity | Invalid project shadows, explicit scopes, duplicate IDs, unrelated-source isolation and damaged compatibility-profile catalog recovery |
| R-06 empty/missing/delete | Runtime empty/missing singleton distinctions, repository seed history/deletion tests, manager clear/delete and referential repair |
| R-07 registrations/exclusions | Typed consumer test and registration-derived manager validation; manually retained context-curator, memory, schema, provider, structural and selfdev exclusions |
| R-08 global store | Real repository initialization, permissions, attempt/receipt recovery, damage block, explicit seed recreation and current live-store fingerprint |
| R-09 project modes | Real submodule/external/standalone setup and recovery, conventional discovery, explicit configuration and branch identity tests |
| R-10 Git operations | Complete repository suite and manager mutation probe: scoped index, unrelated state, history, restore, rename/delete, locks, partial writes and explicit local-bare-remote synchronization |
| R-11 legacy/ecosystem | Exact import receipts, no duplicate source contribution, global-before-project AGENTS.md, Copy and external-source tests; live legacy/ecosystem manager journey |
| R-12 inspection | Complete inspection and TUI manager families, activated read-only paging/history/preview/invalid-source probe, exact immutable text reconstruction |
| R-13 editing | Complete management/draft/repository families and activated separate-home editor/Save/recovery/UX probes |
| R-14 compatibility/kernel | One-time original/current production-renderer equality and human review of unchanged managed kernel and profile prose |
| R-15 freezing | Primary recording-provider tests plus lifecycle probe editing source between requests and reconstructing the daemon without replacing stored text |
| R-16 initial callers | Explicit/default/scoped composer tests, shared-server primary matrix, local/remote command tests, activated CLI/REPL/Harness creation, invalid selections and no-orphan checks |
| R-17 continuation/new context | Resume/reconnect/takeover/split/clear/transfer tests, exact saved text and Startup Context receipts, activated split/restart/clear journey |
| R-18 ordinary switch | App-core and TUI profile tests plus live append-only history comparison, no immediate inference, same-agent no-op and unchanged active skill/model/route |
| R-19 replacement | Candidate preflight/persistence rollback, tool lock and continuation attribution tests; live latest-source replacement preserving skill and making no immediate request |
| R-20 context/rewind | Protected active-profile validation, context preview/draft/apply tests, active lock in live snapshot, pre-curator rejection, rewind/undo and superseded-profile behavior |
| R-21 exports/replay | Session export tests, replay and protocol/SDK suites; complete instruction event and profile content through canonical public replay and explicit remote inspection |
| R-22 skills | Complete source/copy/activation suites, latest external bytes, binary/mode package tests, live source-edit/reinvoke/split/restart/clear behavior |
| R-23 notifications | 38 row dispositions and 407 one-time equality cases, occurrence/source/empty/failure tests, typed todo origin, server-owned recipient scope and Startup Context tests |
| R-24 workflows | 51 row dispositions, 354 one-time cases with named exceptions, command/SDK/transfer/review/ambient/overnight/task-control owners and failure tests |
| R-25 migration evidence | Final manual ledger reconciliation, validated artifact references/hashes, one-time final core render and preserved-whitespace accounting |
| R-26 roster schema/content | Global TOML parser, accepted five aliases/routes/efforts and human descriptions, complete structured manager editing and invalid-source repair |
| R-27 resolver | Complete roster/provider suites and root native/named/pinned runtime tests: candidate order, errors, overrides, concrete persistence and source-free restore |
| R-28 failures/recovery | Caller probes, staged primary construction/rollback, damaged-source catalog recovery, manager stale/crash/reconnect receipts, explicit absent-file-only HEAD fallback primitive |
| R-29 cache/prerequisites | Stable request bytes, append versus replacement, active skill, late-MCP/tool lock, Anthropic split/cache, OpenAI continuation, context-core, Startup Context and disabled-memory regressions |
| R-30 shipped docs | `INSTRUCTIONS.md`, migration, manager, profile, skill, store, notification, workflow, roster and runtime guides; links and current-source review |
| R-31 repository/platform | Exact commits, protected hashes, full range review, dependency review, formatting, strict affected-target lint, root all-target check and final activated identity/canary |
| R-32 future handoffs | Explicit Phase 4/5/6/7/9/10 interface and safe-pre-adoption table in `INSTRUCTION_RUNTIME.md`; no implicit current caller adoption |

## Combined journey map

Each journey requires a final observed result, not only the supporting package
report. The relevant acceptance routes are:

| Journey | Combined route |
|---|---|
| J-01 first use | Real empty-home composer/repository tests, activated daemon and caller probes |
| J-02 damage | Real damaged/missing store and recovery tests, manager repository recovery, live damaged-profile catalog |
| J-03 submodule | Real parent/submodule setup, scoped Save and parent gitlink/index preservation tests |
| J-04 external/non-Git | Real external clone/attach/standalone, missing-checkout and remote failure/recovery tests |
| J-05 legacy/AGENTS.md | Complete import/compatibility tests and live manager import/ecosystem edit with parent HEAD/index unchanged |
| J-06 primary selection | Composer precedence plus every primary caller test, activated CLI/REPL/Harness and local/remote TUI input |
| J-07 static freeze | Managed Save and disk edits between recorded turns, daemon reconstruction and new-context refresh |
| J-08 provisional switch | Live pre-dispatch selection, no append/inference, typed control and local/remote tests |
| J-09 append switch | Live canonical history prefix and complete profile, same-agent no-op, busy-boundary tests |
| J-10 replacement | Live complete current source and no immediate inference, production preflight/continuation/cache attribution tests |
| J-11 context/rewind | Live locked row and rejected range, pin/undo, context apply/revert/reapply and stale-protection tests |
| J-12 lifecycle | Production resume/reconnect/takeover/reload/split/clear/transfer/provider-change and unpublished cleanup tests, daemon restart |
| J-13 skills | Managed/external Copy and complete packages, live frozen invocation/reinvoke/split/restart/clear |
| J-14 occurrence | Notification and typed todo/recipient-scope tests, exact prior-message preservation, empty/invalid-source failures |
| J-15 workflows | Recorded one-time migration plus final production-owner and SDK/command/transfer/review failure tests |
| J-16 inspection UI | Activated complete paging/history/comparison/preview, physical keys/mouse and wide/80/60/extreme-narrow views |
| J-17 editing UI | Activated editor/automatic review/explicit Save, setup/sync and retained client/server recovery, complete package/history tests |
| J-18 roster | Final global policy and independent constructor/resolver/restore tests, manager forms and human review |
| J-19 exports | Canonical replay and complete instruction/status export, raw/Markdown and remote detail mechanism tests |
| J-20 pressure/cache | Large complete synthetic renders, real preflight block/rollback, provider builder shapes, locked tools and deliberate transitions |

## Observed implementation and verification

The original baseline was source `01f1c3c7a`, runtime
`5811f10da-dirty-fa5642aab7fd`. Backend repairs are committed through `e8035d94b`. The final repository-operation
label correction is `c435880fb`, followed by formatting-only `be84ad6ff`. Final
activation identity is recorded in the runtime evidence after build/reload.

### Integrated repairs

- `d976f7bb3`: an invalid unselected compatibility profile no longer hides all
  other valid agents from the picker. Explicit/default selection and manifest
  damage remain fail-closed. Synthetic production-composer red/green evidence
  and activated catalog recovery exercise this boundary.
- `26e4fa719`, `4bb32574b`, `85fd3da7f`, `e8035d94b`: repeated Harness creation
  prepares a genuinely fresh primary using current project/defaults/selector,
  preserves old peer attachments, rejects busy/stale/overlapping transitions,
  requires both successful Subscribe completion and matching state before
  reporting Attached, and treats same-session reattachment idempotently.
  Both reply orders, creation rejection, old-session preservation and atomic
  attachment ownership have focused tests and activated public API evidence.
- `6191ca198`: remaining unprofiled compatibility consumers use managed source,
  not inactive originals or embedded active prose. Their previous full/split
  order and lifecycle are retained. Missing imported common destinations now
  block rather than silently disappearing. Twenty-four complete migration
  outputs match exactly; the disposable capture test was removed.
- `ead798e88`: immutable snapshot capture uses one literal Git blob batch with
  private disk-backed I/O. Complete validation, object/path identity, binary
  bytes and source/index safety remain. A 1,700-file production snapshot took
  472 ms on this machine. That measures snapshot capture, not total Save latency.
- Integration fixtures now use real temporary project directories, project-only
  seeds, synthetic active prompt input, correct request-time routing and isolated
  provider configuration. First-install preparation is outside unchanged
  streaming/recovery timing assertions. No provider budget or memory policy was
  loosened to make tests pass.

### Combined matrix evidence

`~/.jcode/scratch/wp11/signoff-matrix/run-8lCFsS6l/` contains the complete
combined-source matrix and independent result logs. It actually ran, including
failed aggregates:

- Base: 1,487 passed, 16 failed, 1 ignored. Twelve failures expect enabled
  memory/goal storage; three use cwd-sensitive session-context expectations and
  one is the related skill cwd fixture. These documented baseline failures
  remain reported, not renamed as passing. Fresh skill (38), roster (8), prompt
  (19), transfer (4) and Startup Context (55) families passed.
- App-core: 1,404 passed, 4 failed, 25 ignored. Three failures expect enabled
  goal/memory storage. The fourth projection fixture unintentionally loaded
  editable instructions into a fixed 1,000-token test budget. It now installs
  synthetic exact session instructions and passes with the original provider
  budget and all projection/no-auto-mutation assertions retained.
- Protocol: 95. Context core: 58. Command risk: 67. Plan: 78. Provider core: 142.
  Claude CLI: 3. Session types: 14. Overnight: 8. Swarm core: 13. Task types: 7.
- OpenRouter: 121 passed, 1 ignored after correcting its isolated config fixture.
- Harness API: 16; bridge: 70. Rust SDK: 10 library, 15 client, 5 lifecycle,
  5 structured-output and 1 doc test. TypeScript typecheck/build and all 47 tests
  passed separately without package installation/publication.
- TUI manager: 48. Profile: 2. Skills: 20. Startup Context: 47. Context Editor:
  97 passed, 1 ignored. Replay: 6. KV cache: 11. TUI message crate: 9.
  Broader command (206 passed/2 failed) and remote (277 passed/1 failed)
  aggregates retain only the documented enabled-memory expectations.
- Root all-target compilation, strict all-target lint across affected domains,
  workspace formatting and working diff checks passed.

Supplemental `provider-checks/` logs verify Anthropic cached-prefix formatting
(24 tests), OAuth/non-OAuth split shape (2), OpenAI continuation invalidation and
verbatim system delivery (1 each), root concrete native/named/pinned roster
construction and restore (2), and context-window policy (6). No paid inference
or inference-availability guarantee is inferred from those checks.

Final changed-owner rechecks are under `~/.jcode/scratch/wp11/final-regressions/`:
127 instruction tests, 21 primary activation/lifecycle tests, 30 client-lifecycle
tests, the exact fixed-budget projection regression, 58 context-core tests,
complete Harness/bridge/SDK suites and 48 manager tests passed. Root all-target
compilation and strict root/base/app-core/bridge/TUI lint passed. The runner
reported a formatting-only delimiter-layout difference after all substantive
checks passed; workspace formatting and diff checks then passed after correction.
The aggregate runner's original exit status remains recorded as failed, rather
than inventing a later all-green rerun. Earlier failed aggregates and activated
probes likewise remain separate evidence.

### Activated production journeys

All fixtures use the activated binary, real session loading and local recording
providers. Paths below are under `~/.jcode/scratch/wp11/`:

| Artifact | Concrete observed boundary |
|---|---|
| `final-reattach-callers/live-ss40wu9s/caller-result.json` | Final `e8035d94b` public CLI/REPL/Harness: explicit agent, invalid no-orphan, repeated distinct creation, fresh project/source/defaults, same-session reattach and unchanged peer session; zero inference |
| `lifecycle-fixed/live-9uhjyg2w/lifecycle-result.json` and later `activated-lifecycle/` | Managed Save versus frozen system/skill, append/no-op, Context Editor lock and pre-curator rejection, rewind/undo, split, reinvocation, replacement, daemon reconstruction and clear; three localhost requests |
| `manager-inspect-fixed/live-2sorwv41/` | Exact 226-row catalog, 55 content pages, source/index/session immutability, physical keyboard/mouse and named actions/history/Back at 150×40, 80×24, 60×24 and 24×10; reconnect/restart; zero inference |
| `resized-mutations/live-pfu83tr8/mutation-evidence.json` | Final `e8035d94b` complete mutations, binary packages, legacy and ecosystem edits, stale draft recovery, physical editor failure/retry/Save, repository review/cancel/apply, all four sizes by actual PTY resize, real client-restart unsent-form recovery and server-restart draft recovery; one localhost bootstrap and zero manager inference |
| `ux-diagnostic/` and accepted predecessor/final UX probes | Type-first scope/source identity, direct editor and automatic review, explicit Save, cross-scope Copy/collision, external names and responsive source/snapshot/repository pages |
| `live-store-inspection.json` | Read-only production-manager inspection of Mirza's actual global store: 219 global catalog rows, exact active instructions unchanged |

### Preserved state and human review

The active instruction repository remains main at `c0af670a32ff2156693f27a2dab48d4dcb0043d2`,
schema 1 and seed 27, with 213 unchanged tracked files and its original untracked
`.DS_Store`. No instruction remote or real project setup was changed. Both the
store root and `.git` are owner-only; ordinary Git-internal file modes do not
grant access through those private directories. Thirteen dormant memory files
and the five protected checkout files retain their baseline hashes. Current
changed-document navigation passed 197 relative link checks.

Mirza approved the unchanged kernel, transition, replacement and roster text in
WP11's side-panel review on 2026-09-08 at 17:20 UTC. This is central-prose approval,
not a claim that deterministic tests judge it and not final WP11 acceptance.

## Evidence and external boundaries

- Validation is macOS arm64. Other operating systems and every hosted provider
  route are not claimed runtime-tested. Git synchronization uses real local bare
  remotes. Future isolated/async callers receive typed primitives and ownership
  contracts, not an already implemented delegation/job system.
- The curated SDK has no direct skill-activation control. The capability ledger
  records that actual gap; TUI/REPL invocation remains supported. No unapproved
  public API was added merely to make a ledger green.
- Complete phase `git diff --check` reports six migrated workflow assets with
  intentionally preserved literal trailing/newline bytes. Their equality artifacts
  and rationale are in `instruction-inventory/WP11_RECONCILIATION.md`. WP11's own
  range is checked separately. Prompt bytes were not trimmed to silence Git.
- Some original physical probes timed out or hit ENOSPC. Captured native process
  monitoring identified macOS `syspolicyd` reaching approximately 8.3 GiB during
  repeated executable launches, while the active executable's signature verified
  successfully. This was not attributed to Mirza's applications after isolation.
  No system security was disabled. The probe now resizes one client through all
  required widths and retains genuine client/server restart recovery. Its complete
  successful run does not erase earlier failures. Inactive incremental caches were
  removed to recover disk headroom; source, binaries, sessions and instruction
  history were preserved. Owned orphaned fixture daemons were stopped explicitly.
- Incorrect probe assumptions about replay schema and terminal response events
  were corrected against actual production source. A probe correction is not
  represented as a product fix. Resource-failed/interrupted tests are not passing
  evidence.

Final package candidate approval and independent Phase 3 closeout remain pending.
No known in-scope product defect remains in the candidate; reported platform,
provider, baseline-test and system-service boundaries remain explicit.

## Final activated candidate boundary

The final activated candidate is source `642275def6e976cf7bd8aa68958ab15bdf01643a`,
tree `ee2ec1efa55ba409866efc87730ce2f997e5e74e`, runtime
`642275def-dirty-dffcd9d0a568`, with equal current/shared channels, passed canary
and SocketReady. [The compact final evidence](instruction-inventory/WP11_FINAL_EVIDENCE.json)
records actual final-binary caller, lifecycle and complete manager results.

All three passed on that binary. The full manager run includes editor failure and
retry, explicit Save, package/import/ecosystem/stale recovery, physical mouse and
keyboard, all four sizes via resize, client-restart unsent-form recovery and
server-restart draft recovery. The final isolated run is
`~/.jcode/scratch/wp11/final-isolated-manager/live-67gyoqcz`. A preceding serial
repeat passed its caller/lifecycle and manager operations but timed out during
client startup; it remains a failed repeat, not erased by the successful run.

Final runtime checks did not change any of the 213 tracked instruction files,
13 dormant memory files or five protected checkout files. Later evidence or
approval-only commits are distinguished from this activated implementation.
This is candidate completion only. Mirza's WP11 approval is still required.
