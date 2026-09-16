# Combined Phase 4 verification

This is the WP-06 maintainer map for the combined implementation. WP-06 candidate
acceptance and independent phase acceptance are separate human decisions. Earlier
work-package counts remain revision-bound evidence, not an all-green claim for
current source. Final run results belong in the exact-source evidence record.

The subsequent [independent closeout ledger](PHASE4_CLOSEOUT_ACCEPTANCE.md)
records the final phase-wide rerun, requirement reconciliation and fixture repairs.
Mirza accepted the independently verified Phase 4 result on 2026-09-16.
The dated WP-06 observations below remain supporting history, not the final
phase acceptance boundary.

## Requirement routes

| Requirements | Production owners and repeatable checks |
|---|---|
| A01 | `swarm_retirement` in base, app-core and TUI. Verify direct/alias/batch, fresh/attached protocol, restoration and explicit off controls. No live enabled Swarm. |
| A02–A06 | `instruction::composition`, `model_roster::`, app-core `delegation`, `agent::isolated::tests`, and `agent::startup_context::tests`. Verify preparation failures, exact model/settings, payload authority and current/custom Startup Context. |
| A07–A08 | Child policy and immutable Registry permission fixtures in the delegation/isolated families, MCP `access`/pool/manager tests and direct administrative negatives. Read-only is not an OS sandbox. |
| A09–A13 | Real hosted delegation/FIFO/admission/Stop/reload tests, execution store recovery and native child journey. Original-parent ownership is separate from continuation read access. |
| A14–A16 | [Complete producer ledger](EXECUTION_PRODUCERS.md), app-core `tool::tests::reliable_execution`, execution ingress, MCP request/rich results, native stream and mutation tests. Check producers before the shared formatter. |
| A17–A20 | Source/managed/part-reader tests, PDF/image tests and native pressure workflow. Preserve exact acquired bytes and one-effect counters. Source reads do not archive full plain files. |
| A21–A23 | Session capture and execution snapshot tests, including large shared blobs, narrow reads with unrelated blobs unavailable, independent writers and newest-two pruning. |
| A24–A26 | Execution storage/retention fault tests and native tiny Active fixture. Wrong UUID/offline/deletion outcomes are explicit. No user archive is modified as a probe. |
| A27–A30 | Runtime/native-worker/control and provider/MCP transport tests, real SQLite cross-process tests, historical migration fixtures and native restart/Stop journeys. Acknowledgement is not quiescence. |
| A31 | 200,000-record sparse/deep task pages, exact mixed-state ordering and SQL full-scan/sort counters; unavailable payload tests; large snapshot metadata/narrow reads; existing one-body-worker TUI tests. |
| A32–A33 | Native standalone JSON/NDJSON/REPL, shared daemon/Harness, both SDK suites, frozen input-binding collisions and capability negatives. Server paths are not assumed client-mounted. |
| A34–A36 | Native PTY task-monitor script plus actual local/remote input and human-child transaction tests. Verify wide/narrow/minimum layout, pinned selection, readable input, follow/pause, controls, cleanup and child apply/undo without chat/restart. |
| A37 | Session export/redaction and source-equality tests, private storage/control credential checks, real volume metadata and native fixture safeguards. No implicit whole-archive export. |
| A38 | Session, Startup Context, profile, roster, skills and context-core families, disabled-memory negatives and original prompt/tool-lock/continuation checks. No skill-lifetime or context architecture redesign. |
| A39 | Current guides, literal link checks, real configuration, full diffs/commits, coordinated build/reload, running/current/shared identity and canary, then native smoke. |
| A40 | D-26 framework-stage prose review and human field acceptance. Synthetic content tests mechanism only. Final roles/skills remain later program work. |

## Native journeys

All commands use a built TUI/CLI candidate, private state and localhost scripted
inference. They are not hosted model evaluations or delegated implementation.
Use `scripts/run_isolated_test.py` so HOME fallbacks and explicit endpoints cannot
reach the live namespace. Never substitute the live state directory for a fixture.

```sh
python3 scripts/run_isolated_test.py python3 -B scripts/verify_isolated_delegation.py \
  --binary "$CANDIDATE" --evidence-parent "$EVIDENCE"
python3 scripts/run_isolated_test.py python3 -B scripts/verify_retention_journeys.py \
  --binary "$CANDIDATE" --evidence-parent "$EVIDENCE" --mode daemon
python3 scripts/run_isolated_test.py python3 -B scripts/verify_execution_pressure.py \
  --binary "$CANDIDATE" --evidence-parent "$EVIDENCE"
python3 scripts/run_isolated_test.py python3 -B scripts/verify_task_monitor.py \
  --binary "$CANDIDATE" --evidence-parent "$EVIDENCE" \
  --fixture-test-binary "$BASE_TEST_BINARY"
```

| Journey | Combined route |
|---|---|
| J01 | Native catalog → child → parent outline/tool expansion → artifact → ordinary retained reply. |
| J02 | Native explicit permission/preset change → omitted follow-up, exact directive IDs and unchanged system/history prefix. |
| J03 | Hosted FIFO, simultaneous capacity, targeted cancellation and explicit follow-up tests, plus native interrupted child/queued input. |
| J04 | Native large captured output → withheld delivery → explicit existing context transaction → saved-tail read with one original effect; exact read/batch tests. |
| J05 | Native monitor Stop/background/force, shared background-wait and adopted-owner tests, retained partial output and completed effects. |
| J06 | Native killed-host/queued-child recovery and source-free restore after alias/profile edits, plus normal reload-quiescence tests. |
| J07 | Native immutable snapshots across rewind/restart and automatic reader pruning, unchanged retained transcript. |
| J08 | Native cold Active archival/read-in-place and exact reviewed fixture cleanup, plus fault/wrong-volume/offline tests. |
| J09 | Actual PTY keys/mouse/resize for monitor and idle-child Context Editor, plus local input/worker route and busy/stale/target-correlation tests. |
| J10 | Native standalone JSON/NDJSON/REPL and TypeScript/Harness, host autostart/environment contract and version/namespace failure tests. |

`verify_retention_journeys.py` also supports `--mode json`, `ndjson` and `repl`
for direct inspection callers. The pressure script uses the real explicit
context-transaction backend, not physical user input or proof that compaction was
economically necessary. The task-monitor script supplies physical PTY evidence.

## Compatibility, performance and safety bounds

Execution schema 21 is an index-only upgrade of schema 20. Preserve available
legacy bytes and reject unsupported owners rather than infer safe process control.
Harness v1.6 adds body-free `attachable` session metadata so SDK global event
discovery skips isolated children without hiding them from inspection.
The historical schema-18 fixture verifies the retention observation floor against
actual old DDL, not a modern database with a falsely lowered version.

Task pages are bounded indexed partitions. Cost can still grow with the number of
applicable child conversations, which have no TTL. Full snapshot creation reads
and projects a coherent conversation, so it is not constant-time. Snapshot metadata
does not require instruction/message bodies. Narrow transcript reads skip unrelated
message bodies but retain the established captured-active-instructions section.
Catalog validation intentionally reads/renders current instruction sources to
report invalid entries, unlike metadata-only session status.

Retain the failed aggregate inventory in [EXECUTION_FAILURE_TRIAGE.md](EXECUTION_FAILURE_TRIAGE.md).
A new active-path failure must be investigated, not assigned that historical
exemption. Zero selected tests, skipped native fixtures, watchdog interventions,
failed compilation and untested platforms remain distinct from passing evidence.

macOS arm64 is mandatory native coverage. Same-machine networked PTY is not an
external SSH host. Portable Linux/Windows type compilation is not native runtime
parity. Active remains unencrypted and ownership-ignored under the accepted policy.
Local files and credentials retain their existing protections. No service-specific
remote compensation, hosted billing/cache-hit rate or prose-quality guarantee is
implied by these checks.

## Observed combined candidate evidence, 2026-09-15

The private native candidate at implementation `80dba73c4` passed the expanded
58-request localhost child journey, including parent tool expansion, changed and
omitted settings, independent batched children, SDK global event discovery with
seven non-attachable children, crash/queued-input recovery and exact source-free
restore. Standalone JSON, NDJSON and REPL remain valid. None of those requests
used hosted inference.

The same candidate passed native pressure retrieval: 1,000,015 retained bytes,
one original command effect, withheld delivery, one explicit existing context
transaction and later saved-tail read. The retention journey used five localhost
requests, preserved snapshots through rewind/restart, automatically archived ten
owned outputs, pruned to two snapshots and completed exact reviewed cleanup.

The physical monitor journey passed all five sizes, readable input tabs,
Stop/background/force, idle-child curator/apply/undo, reconnect and tiny Active
cleanup. Its owned-work cleanup report was empty. Recorded narrow input and
child-editor frames were inspected, with no reported frame anomalies.

The 38-group Rust matrix completed with 35 passing groups and three failed groups.
The failures were an obsolete post-promotion terminal-error assertion and its
leaked fixture control, and five missing capability-ledger dispositions. Corrected
promotion/reload tests passed all 33 cases, and the complete Harness/SDK groups
passed again. The initial failed aggregates remain retained, not rewritten green.
A separate execution/storage suite passed 91 tests with three explicit native
fixtures ignored in that aggregate. The real-volume monitor/retention journeys
supply separate native evidence. Catalog acquisition tests passed 34 cases.

Additional checks cover historical schema-18 migration, lossless schema-20 to 21
index upgrade, body-free snapshot metadata, narrow message reads, 200,000-record
sparse/deep task queries, private export/state preservation and Linux/Windows
portable contracts. Strict affected library/test Clippy passed. No new external
dependency was added.

Failed attempts remain evidence: the new SQL fixture initially used an unsupported
`usize` binding; a snapshot test incorrectly omitted instructions from a transcript
contract that explicitly includes them; a native request-count edit changed reply
IDs accidentally; and long native TMPDIRs exposed a real execution-control socket
length failure. Fixtures were corrected and the socket owner repaired. No valid
behavior assertion or protected context policy was relaxed.

Exact private commands, logs, native request/frame records, source hashes and
preservation receipts are indexed under `phase04-wp06-20260915` in the program's
scratch evidence. Final shared activation and work-package acceptance are recorded
separately in the program candidate/progress report, not inferred from these
private candidate runs. Independent phase closeout is still required.
