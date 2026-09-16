# Independent Phase 4 closeout verification

**Status:** Phase 4 accepted by Mirza on 2026-09-16 at 16:27:11.167 UTC. The exact accepted implementation is `f2ea363c4013107aa3d8394df2eb345353057720`, activated as `f2ea363c4-dirty-c80238522ec5`, with running/current/shared equality and passed canary. It was fast-forwarded to local and actual downstream main at 16:28:16 UTC. This acceptance update is documentation-only.

## Reviewed boundary

The independent closeout began at `6c0890311d7bfa4df4cba3952b13e138fc1808df`, following all six accepted work packages. The complete phase range starts at `5320ea4c5f41939b16b6a02edd13fef03994baad`. At the opening boundary it contained 113 commits across 393 files, with 55,485 insertions and 4,657 deletions. Review reconciled the complete commit/path inventory with the accepted scope and traced final production owners across delegation, execution, storage, inspection, provider/MCP adapters, protocol/SDK and TUI consumers. It did not infer combined correctness from package acceptance counts.

The protected context-control architecture, Startup Context authority, disabled memory and Swarm, model roster and skill lifetime remain unchanged. Existing curator/projection/transaction owners supply child editing. No final prompt corpus, scheduler, desktop change, hosted inference, delegated review or scheduled implementation was introduced.

## Independent findings and repairs

The 44-group final-source matrix completed with 42 passing groups and two failures:

1. `apply_claim_pins_live_lease_and_recovery_guard_serializes_named_coordinators` used a 20 ms real-time lease for both an expiry check and subsequent durable queue/close operations. The latter could outlive the lease and correctly receive `LeaseNotFound`. The fixture now ages its own lease explicitly for the pinning assertion and uses an ordinary 30-second lease for subsequent persistence. It still proves non-expiry while claimed, expiry after release, cross-coordinator exclusion and successful recovery.
2. `build_queues_background_tasks_and_reports_queue_status` assumed its 200 ms simulated producer remained active until status inspection. The producer could finish first, correctly removing the queue section. A fixture-owned build lock now holds work queued through inspection, then releases it for real completion. The adjacent deduplication/status fixture uses the same synchronization. Original queue, watcher, identity and terminal-outcome assertions remain.

Commit `e57930cc0` contains only these test changes. No production behavior or protected architecture was weakened. Each repaired case and the adjacent deduplication case passed three exact repetitions. The complete context/Startup Context family then passed 138 tests with one explicitly ignored measurement, and the full selfdev family passed 38 tests. Formatting and strict app-core library/test Clippy passed. The original failed runs remain retained evidence.

The supplemental matrix passed 12 of 13 groups. The remaining group was an erroneous child-policy filter matching zero tests, not a product assertion failure. The wrapper rejected it. Exact immutable-permission and isolated-Registry-lifetime selectors replaced that invocation and each passed one test in coordinated run `run-1458800ef88d4d430c9e8a96cdd7b2c441ee816434ed3e225266cd5ab5e9075e`. Do not describe the original aggregate as green.

### Final field findings

Actual in-flight `bg wait` twice reported a cancelled control task when a native
background command had completed successfully. Already-terminal retrieval worked.
The native worker could seal its durable result and then shut down while its control
handler performed the final metadata read. The client reconciled transport errors
with durable state, but returned an `Unavailable` protocol reply without that same
check. Commit `3d2054994` applies the existing durable-truth reconciliation to that
reply too. A deterministic real-store/socket regression first failed, then passed
for completed, failed, cancelled and interrupted outcomes. A still-running case
remains unavailable, not falsely terminal. No producer is repeated and no error
message is parsed to infer completion.

The same review found direct file reads in managed background output consumers,
bypassing the shared archive/deletion/chunk-integrity owner. A same-length changed
output regression reproduced that defect. Commit `67b59a04c` routes bg output/tail,
compatibility output and completion previews through the existing verified reader.
It retains bounded streaming, exact selected tails and cancellation. The regression
and eight affected groups passed in `bg-fix-01`, including base/app execution,
Registry execution, bg tools, background integration and formatting. The initial
fixture attempt omitted the required start transition and failed at ownership
validation; the corrected red run reproduced the intended integrity failure.

Commit `b95db8179` extends the native child/CLI verifier with three ordinary
in-flight background waits and one-effect counters. The fixture checks that each
command is still running before the wait starts. The expanded journey uses 67
scripted localhost responses rather than the earlier 58.

### Verification disk-pressure incident

An intervening `wait-fix-01` run passed its five test groups but exhausted the
internal filesystem during strict lint. The compiler recorded ENOSPC, and live
database-open errors were observed by both the agent and Mirza. That run is
interrupted evidence, not a passing lint or matrix. Only manifested inactive
incremental caches were removed. The blocked queued cleanup was cancelled before
the exact-manifest fallback, after verifying no active compiler. Read-only SQLite
quick_check returned `ok`, schema21 and configuration/instruction identities were
preserved. The exact failed session-owned task was recovered as interrupted with
its input/prefix intact after authenticated owner inspection, absent process-group
proof and exclusive output-lease acquisition. No successful result was invented.

Subsequent compiler work uses a 3-GiB free-space stop guard and reviewed cache
cleanup between compiler slices. Output reserves cannot reserve disk against
unrelated compiler writes. Exact incident/recovery before-images and receipts are
under `disk-incident-20260916/`; the program's incident report retains the human
observation and operational recovery. Final strict lint and activation are separate
gates after that incident. Strict base/app-core library/test Clippy, full formatting
and diff checks passed in coordinated run
`run-533ec2abdb3a74ca9356b3e916abcf62c84beb86205a1bf1b0bb0a8689d2afe8`
with the free-space guard and no guard-triggered stop.

## Requirement-to-evidence ledger

`Validated` denotes production-owner mechanism/integration tests. `Verified` identifies the native public workflow where exercised. Synthetic content is used for mechanics, never to grade instructions.

| ID | Final implementation and independent check | Evidence boundary |
|---|---|---|
| A01 | Global availability, registration/alias/batch/protocol/restoration gates. Base, app-core and TUI retirement families passed; native tools omit memory/Swarm. | Validated plus native preservation |
| A02 | Explicit request normalization, isolated availability and preparation cleanup through composer, hosted delegation and isolated-Agent tests; native creation. | Validated and native verified |
| A03 | Independent roster/factory resolution and concrete Session identity; eight roster tests and native source-free restore after edited aliases/profiles. | Validated and native verified |
| A04 | Omitted effort versus explicit values through existing roster resolver and request types. No parent provider mutation. | Validated |
| A05 | Normal/common-child system composition and user-authority presets; 22 composition tests and recorded native changed/omitted-setting payloads preserve the system/history prefix. | Validated and native verified |
| A06 | Fresh/default/custom/external/empty/disabled captures through existing engine; 55 base and 21 app-core startup checks, hosted failure/no-orphan and unchanged-default tests. | Validated |
| A07 | Child policy plus real mutation paths check artifact targets, traversal/symlink/moves/all destinations; native artifact creation and immutable original-turn Registry permission tests. | Validated and native verified |
| A08 | Child administration/recursion, initiative mutation and MCP classification/blocklist enforcement; hosted/isolated, 62 MCP tests and native forbidden child chat. | Validated and native verified |
| A09 | Separate original-parent/continuation identities, Session persistence, resume/split/clear/transfer policy and native exact restart recovery without duplication. | Validated and native verified |
| A10 | Shared execution owns one foreground result or background receipt, explicit notify/wake and promotion. Native waiting-parent and CLI/SDK paths passed. | Validated and native verified |
| A11 | Transactional busy-child opt-in FIFO, start-time settings and no same-preset reapplication through real store and host tests. | Validated |
| A12 | Running Stop/failure cancels unstarted cohort input, selected queued Stop is local, partials remain. Native killed-host recovery and explicit follow-up passed without replay. | Validated and native verified |
| A13 | Real-store synchronized admission admits exactly fifteen of twenty starts, excludes idle history and retains one FIFO reservation. Hosted capacity rejection has no dispatch. | Validated |
| A14 | [Complete producer inventory](EXECUTION_PRODUCERS.md) reconciled with registrations and adapter boundaries. Registry, batch, helpers, acquisition, SDK ingress and native sentinel retrieval checks passed. | Validated plus native text paths |
| A15 | Raw stdout/stderr, split/invalid UTF-8, rich MCP/media/error and adapter acquisition checks through execution, MCP and native-tool families. | Validated, not hosted vision quality |
| A16 | Scoped invocation ancestry, replay conflict checks, nested batches and media references through reliable-execution tests and native two-child batch. | Validated and native verified |
| A17 | Unsafe legacy retry rejected before effects. Native pressure workflow retained 1,000,015 bytes, explicitly edited context and retrieved the tail with one original command effect. | Native verified |
| A18 | Shared Unicode character policy, aliases/defaults, boundary ties/EOF, long lines, CRLF, formatting overhead and tiny budgets through tool types/core, reader and config tests. | Validated |
| A19 | Version-bound source and logical managed-output points; stale/replaced/cross-source rejection, exact reassembly, withheld retry and no full plain-source archive. | Validated plus native retrieval |
| A20 | PDF derived-text and atomic image paths through the read family, part/integrity tests and retained media checks. | Validated, provider media limits explicit |
| A21 | Session storage leases and strict checkpoint/journal capture, content-addressed immutable snapshots, busy inspection and restart/rewind stability. | Validated and native verified |
| A22 | Existing projection/provenance owner supplies whole summary intersections, explicit raw reads and snapshot-bound prefixes, with protected child directive IDs. | Validated and native verified |
| A23 | Four snapshots of ten targets retain newest twenty under reader activity; in-flight ownership and source history preservation. Native worker retained newest two across restart. | Validated and native verified |
| A24 | Existing copy/hash/publish/delete journal, fault recovery, UUID/offline checks, stable aliases/read points and in-place archive access. Actual Active fixture passed. | Validated and macOS native verified |
| A25 | Reserve-aware capture, ENOSPC/EIO and interrupted spillover fault tests preserve prefix and reject false success. Actual config retains 1 GiB reserves. | Validated, not real-disk exhaustion |
| A26 | Exact whole-output/overshoot/snapshot-impact review and one confirmation, stale rejection, partial/repeated recovery. Native API and physical monitor fixture cleanup passed. | Validated and native verified |
| A27 | Real execution/native process owners, adopted inner-task cancellation, foreground versus reload causes, background-wait isolation and actual partial-output Stop/force. | Validated and native verified |
| A28 | Request-local lifetime/forwarder guards, real quiet HTTP, persistent WebSocket, HTTP2, CLI process and MCP pending/late-response tests. | Local transports validated, no vendor acknowledgement |
| A29 | Durable terminal witness and proven lost-owner recovery, native worker/restart and child queued cancellation without effect replay. | Validated and native verified |
| A30 | Real SQLite multiprocess tests, schema migration, output/metadata transaction recovery, private permissions and exact terminal receipts. Live schema21/indexes verified read-only. | Validated plus actual migration state |
| A31 | Indexed 200,000-record sparse/deep mixed-state pages, unavailable-body metadata tests, snapshot narrow reads and single in-flight TUI body worker. | Validated, not constant-time full snapshots |
| A32 | Actual standalone JSON/NDJSON/REPL, shared daemon and TypeScript/Harness fan-in, host autostart/environment and public SDK tests. | Native verified on macOS |
| A33 | Frozen schema collision binding, retained legacy false/absent behavior, capability/version/namespace rejection, Rust/TypeScript suites and portable Linux/Windows DTO/API checks. | Validated, not native platform parity |
| A34 | Physical PTY keys/mouse, scope/nesting, pinned completion, readable input/raw receipt, live follow/pause, original-owner Stop/background/force and storage review. | Native verified |
| A35 | Native 140×32, 80×24, 60×24, 48×12 and below-minimum 47×11 frames; focus/input/hit tests and bounded metadata/body work. | Native verified plus mechanism coverage |
| A36 | Native child curator/apply/history/undo through parent monitor, unattached child routing and no chat/restart. Hosted busy/stale/target-race tests preserve parent state and current directives. | Native verified plus race coverage |
| A37 | Structural redacted export and source-equality tests, private store/IPC identities, verified archive UUID and accurately disclosed unencrypted/ownership-ignored policy. | Validated plus live configuration evidence |
| A38 | Session, startup, composition, roster, skills, 58 context-core and 97 TUI editor tests, explicit disabled-memory negatives and runtime/config preservation. | Validated plus native preservation |
| A39 | Current shipped guides, exact phase commits/paths, configuration/schema, source/runtime/channel/canary, documentation links and final activation. External capability-map and phase status updates are recorded by the accepted program completion report. | Verified; final phase accepted |
| A40 | D-26 provisional framework-stage prose and accepted package review retained unchanged. No wording snapshots, phrase grading or automated LLM benchmark added. | Human accepted within provisional framework scope |

## Combined journeys

| Journey | Independent observed route |
|---|---|
| J01 | Native catalog, child creation, parent outline/tool expansion, artifact write and retained reply. |
| J02 | Native explicit permission/preset change, omitted follow-up, same structural notices and unchanged earlier prefix. |
| J03 | Real-store/host FIFO, fifteen-child admission and targeted/cohort Stop tests, native independent child batch and queued crash cancellation followed by deliberate continuation. |
| J04 | Native pressure/one-effect retrieval, exact reader and batch sentinel tests. The context edit uses the actual explicit transaction backend, not a physical-compaction claim. |
| J05 | Native monitor Stop/background/force preserves completed effects and partials; owned-task/adoption/background-wait tests verify actual termination. |
| J06 | Native host crash, queued cancellation and exact child restoration after alias/profile corruption; normal reload and native-worker checks. |
| J07 | Native immutable snapshots across rewind/restart, automatic newest-two pruning and preserved delivered history. |
| J08 | Ten tiny owned outputs archived on UUID-verified Active, read in place and exact reviewed cleanup. Fault/offline/wrong-volume cases use storage fixtures. |
| J09 | Physical PTY monitor and child editor, curator/apply/undo and reconnect at five sizes, plus real local/remote input and stale/busy routing tests. |
| J10 | Native JSON/NDJSON/REPL, TypeScript/Harness, host autostart/environment, namespace/version rejection and seven non-attachable child SDK entries. |

## Commands and evidence

Use [the combined reproduction commands](PHASE4_INTEGRATION_ACCEPTANCE.md#native-journeys), [test state isolation](../../scripts/TEST_STATE_ISOLATION.md), and the owner-specific acceptance guides. Tests run through the coordinated compile owner and private HOME/JCODE_HOME/runtime/XDG/endpoints. Native fixtures use synthetic localhost providers and private state. They are not sub-agent delegation of the closeout work.

Private evidence is retained at `~/.jcode/scratch/phase04-closeout-20260915/`. `matrix.json`, `matrix-01/results.json`, `matrix-repairs.json`, `matrix-repairs-01/results.json`, `supplemental.json`, native logs/results, `BASELINE.json`, `PRESERVATION-AUDIT.json` and `REVIEWED-FRAMES.json` identify exact commands and observed outcomes. Results are not summed into a fictitious unique-test total. A zero-test filter, ignored/manual fixture or failed aggregate is never reported as passing.

The first native rerun used exact activated `6c0890311-dirty-658f4fa15a52`. Child/SDK used 58 scripted responses, pressure four and retention five. The physical monitor passed all controls, child editing, cleanup and reconnect, with an empty unfinished-owned-work cleanup report. Twenty-nine monitor frames and two child-lock frames recorded no anomalies. Representative wide, narrow, minimum, child-context and snapshot-impact frames were independently read.

## Limits retained for acceptance

- Native runtime coverage is macOS arm64. Same-machine networked PTY is not external SSH. Portable Linux arm64/Windows x64 types compile, not the complete native application or platform-specific process/storage behavior.
- Read-only is limited native mutation/MCP policy plus instructions, not an adversarial OS sandbox. Trusted same-user control does not attest physical human origin.
- No hosted-model quota, billing/cache-hit claim, prompt-quality evaluation, vendor-side remote-compute termination or service-specific compensation was attempted.
- Full snapshot creation reads/projects full coherent source. Task cost may grow with applicable durable child conversations. Binary parts use bounded-memory full-part integrity checks, not constant-time random access.
- Historical discarded/unreceived bytes cannot be reconstructed. Unknown owners fail explicitly rather than reexecute effects.
- [Historical failed repository suites](EXECUTION_FAILURE_TRIAGE.md) remain failed evidence. The focused independent matrix does not claim a universally green repository. The named Phase 10 fixture-maintenance handoff remains, without waiving active Phase 4 correctness.
- Active retains its accepted existing protection level. Native cleanup touched only uniquely identified tiny fixture outputs, not user archives. Manifested inactive incremental-cache cleanup preserved binaries, source, sessions, instructions and durable evidence.

Mirza explicitly accepted the final candidate and publication on 2026-09-16 at 16:27:11.167 UTC. The program PHASE_COMPLETION.md is the authoritative downstream rationale and future handoff. This ledger records observed evidence and does not erase the disclosed limits or failed historical runs.

## Final activated acceptance

All four native suites passed again on immutable `f2ea363c4-dirty-c80238522ec5`.
The expanded child/CLI/SDK suite completed 67 localhost requests, including three
in-flight background waits and exact one-effect counters. Pressure retained
1,000,015 bytes with one effect. Retention archived ten owned fixtures and retained
the newest two snapshots. Physical monitor controls, child curator/apply/undo,
cleanup and reconnect passed at five sizes, with 29 monitor and two child-lock
frames reporting no anomalies and no unfinished fixture work. This session also
received a correct terminal result from an actual in-flight bg wait on that native
runner. Final SQLite quick_check returned ok, schema21 and protected file/config/
instruction identities remained intact, and about 6.7 GiB was free.

`FINAL-ACTIVATION.json`, `FINAL-VERIFICATION.json`, `final-runner-results.json` and
`native-final/` retain the final evidence. Source changes after the accepted
implementation are limited to this acceptance/index record. No runtime source,
configuration or instruction asset differs, so the documentation-only tail does
not require another build.
