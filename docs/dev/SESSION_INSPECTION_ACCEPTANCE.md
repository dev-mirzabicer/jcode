# WP-03 inspection and retention acceptance evidence

**State:** Accepted by Mirza on 2026-09-13 at 19:26:02.486 UTC. Exact candidate `6854e1af9` was fast-forwarded to local and downstream main at 19:26:53 UTC. The implementation was verified and activated before acceptance. This is not Phase 4 completion.

**Implementation:** `c2dc31ef44f2407d8eb8823ba0a8c6457a6aacc1` on
`mirza/phase-04-wp-03-inspection-retention`, based on accepted WP-02 `ed3100a3d`.
**Activated:** `c2dc31ef4-dirty-9d6630b54a17`, Jcode v0.75.315-dev.
Running, current and shared identities match. Canary passed. Coordinated reload
reached SocketReady at 18:42:12 UTC. Subsequent commits limited to this evidence
and its documentation links do not change the activated implementation.

## Requirement-to-observation mapping

| Requirement | Concrete evidence | State |
|---|---|---|
| A21: coherent immutable capture, independent of the busy Agent | Session checkpoint/journal lease tests, actual independent-process writer with append/checkpoint handshakes, retired-journal crash replay, unsupported live writer negatives and exited-writer observation. Actual shared-server fixture holds the Agent mutex while outline/transcript/status succeed. Old snapshots remain unchanged after actual native rewind and daemon restart. Content-sharing tests require identical blob references for unchanged source. | Verified on native/public paths, race/corruption boundaries validated |
| A22: projected/raw source and provenance | Real `project_context` owner with synthetic summary, reasoning suppression, distillation, media and active-profile identities. Whole intersecting summary coverage, exact raw source, immutable running-output prefix, original input and legacy availability tests. Activated field inspection of source position 201 returned the complete summary covering 200–263; raw mode returned only position 201. Historical patch expansion retrieved its original input/result without execution. | Verified |
| A23: reader-owned per-target pruning | Four snapshots for each of ten targets leave the newest twenty. Shared read ownership defers pruning. Actual-reader activity is separate from target use and inherited snapshot ownership. Corrupt unreferenced-blob tests preserve committed prune counts, continue independent reclamation, retain shared blobs and recover afterward. Native automatic maintenance prunes four snapshots to the newest two without polling refreshing activity. | Validated; automatic native workflow verified |
| A24: reliable cold archival and recovery | The existing mover is reused. Fault tests cover before copy, after copy, location publication, alias publication and source deletion, with duplicate recovery and unchanged generation. A real, explicitly owned Active fixture verifies different filesystem devices, exact text continuation and snapshot references, in-place reads, wrong UUID/offline failures and no substitute mount. Native automatic maintenance archived ten fixture outputs and preserved old snapshots/read access. | Verified on macOS/Active |
| A26: exact reviewed whole-output cleanup | Exact candidates/total/overshoot, wrong reader/token, changed-content stale review, live/recent-spill exclusions, disclosed snapshot IDs, four deletion fault points, multiple-target partial success, duplicate confirmation and retained input/history checks. Real Active and native trusted-client journeys delete only confirmed fixture output. Resuming a session does not unnecessarily hide already cold-archived outputs. | Verified on native path; faults validated |
| Durable operational activity | Actual model-turn leases and execution transitions, explicit-use hooks, message-only save activity, polling neutrality, namespace-bound completion and fixed lost-owner observation tests. Schema 19's one-time legacy floor preserves recorded activity without inventing a user event. Native status polling leaves the deliberately aged fixture clock unchanged. | Validated; native tracker/worker verified |
| Public tools, protocol and capabilities | All three inspection tools execute through real daemon, standalone JSON, NDJSON and REPL. Harness attachment/response correlation and Rust/TypeScript capability rejection-before-transport tests pass. Same-connection metadata and Stop remain responsive while another inspection is blocked on a Session persistence lease. | Verified |
| Persistence/privacy/prerequisite boundaries | Strict capture never migrates/salvages source. Private blob/index ownership and existing export policy remain. Tests cover raw/source nonmutation and active-profile/media fidelity. All Session regressions and all 58 context-core tests pass. Memory/Swarm remain disabled, instruction source HEAD unchanged, and no new dependency or desktop implementation was introduced. | Validated; live preservation verified |

This matrix maps requirements to observations. Counts below are not substituted
for that mapping or summed into a fictitious unique-test total.

## Final verification groups

- `jcode-base --lib execution::`: **78 passed, 2 ignored**. The ignored cases are
  explicit native fixtures, not silently passing cases. The new native Active
  fixture was run separately. Subprocess helper result lines are not extra unique
  tests in the aggregate.
- `jcode-base --lib session::`: **116 passed, 1 ignored**. The ignored independent
  writer entrypoint is invoked by its passing parent test.
- App-core inspection service: **3 passed**. Shared-server busy/same-client/Stop
  workflow: **1 passed**, through real stream pairs and actual execution owners.
- Full Harness/API/SDK groups: **16, 72, 10, 18, 5 and 5 tests passed**, plus one
  SDK doc-test. Empty documentation targets are not behavioral evidence.
- Final cleanup correction: **5 passed, 1 explicit native fixture ignored**.
  Context-core: **58 passed**.
- TypeScript typecheck, build and **50 tests passed**, including runtime schema
  parity, independent capability negotiation and reply/session correlation.
- Strict Clippy passed on base, app-core, protocol, Harness, bridge, Rust SDK and
  TUI library/test targets. Whole-workspace formatting and complete package-range
  diff checks passed. The root TUI executable compiled through coordinated builds.
- Changed portable tool types and Harness API compiled for Linux arm64 and Windows
  x64. This does not establish full application or native storage/control parity.

Representative repeatable commands:

```sh
scripts/dev_cargo.sh test --profile selfdev -p jcode-base --lib execution:: -- --test-threads=1
scripts/dev_cargo.sh test --profile selfdev -p jcode-base --lib session:: -- --test-threads=1
scripts/dev_cargo.sh test --profile selfdev -p jcode-app-core --lib session_inspection::tests -- --test-threads=1
scripts/dev_cargo.sh test --profile selfdev -p jcode-app-core --lib repeated_harness_creation_is_fresh_and_failure_preserves_the_attached_session -- --test-threads=1
scripts/dev_cargo.sh test --profile selfdev -p jcode-harness-api -p jcode-harness-api-server -p jcode-sdk -- --test-threads=1
scripts/dev_cargo.sh clippy --profile selfdev -p jcode-base -p jcode-app-core -p jcode-protocol -p jcode-harness-api -p jcode-harness-api-server -p jcode-sdk -p jcode-tui --lib --tests -- -D warnings
```

Run environment-sensitive verification with isolated HOME, JCODE_HOME and
JCODE_RUNTIME_DIR, not the live user roots. Use the coordinated self-development
owner. Native archive fixtures require explicit new fixture configuration and
`--ignored`; never point them at the production archive namespace.

## Actual-binary journeys

The private `native-wp03.py` uses an isolated state/runtime/work directory and a
scripted localhost provider. Each daemon/JSON/NDJSON/REPL case performs five local
requests and exactly one original command effect. It invokes `session_outline`,
`read_transcript` and `expand_tool_use`, verifies full saved output, and does not
repeat the producer. JSON/NDJSON stdout remains valid in the corresponding modes.

The daemon additionally:

1. Opens multiple snapshots, reads original source, performs a real explicit rewind
   and verifies the old snapshot remains identical.
2. Restarts its actual daemon/bridge, reattaches and retrieves that same snapshot.
3. Ages only its private fixture clock, then observes the production 60-second
   maintenance worker archive ten outputs and prune two of four snapshots.
4. Polls status without changing the activity clock, reads archived data in place,
   and receives explicit failure for a pruned snapshot.
5. Reviews one exact archived output with snapshot impact, rejects a wrong
   confirmation, performs one correct confirmation, retries idempotently and
   receives deliberate-deletion failure on later expansion.

All fixture archive directories were removed. No user output was aged, moved or
deleted as a probe. No hosted inference, model benchmark, delegated child or
schedule was used. Native subprocesses were ordinary test executables, not agents.

After shared activation, this real busy work-package session successfully opened
an immutable snapshot of **558 source messages** at context revision 2. The complete
outline was retained, with only the requested presentation prefix delivered.
Actual compacted-summary intersection, raw source and historical patch expansion
then completed successfully. Context-pressure withholding preserved all three
results; they were recovered from their saved files without repeating the calls.

## Migration and live-state verification

Live execution metadata migrated from schema 17 to **19**. Three legacy activity
rows received a fixed observation floor of `1789929731`; subsequent reads did not
refresh that floor. The existing archived output count remained one. Live config
SHA-256 stayed
`c039bddd40c612379e6d8a0eb46bc3c1d054c51452a6494b59658b6a4d59ed2d`.
Memory and Swarm flags remain false. No volume setting or active instruction prose
was changed. The instruction repository remains at
`c0af670a32ff2156693f27a2dab48d4dcb0043d2`.

Active's existing unencrypted/ownership-ignored protection is the accepted boundary.
Cleanup trusts same-user clients, requires an exact preview plus one confirmation,
and does not attempt to attest physical-human origin. These are deliberate product
semantics, not a claim of adversarial filesystem isolation.

## Failed attempts and corrections

Failures remain evidence rather than rewritten success:

- A streaming fixture initially returned inline output after writing capture.
  Corrected it to return its retained reference, preserving production checks.
- The real creation fixture found an unpublished persistence-lock artifact.
  The cleanup owner now removes that unpublished artifact; its existing assertion
  was not weakened. Published-session lock files are not removed routinely.
- The independent-process fixture exposed an exited but unreaped writer PID.
  Rare unowned-capability checks now distinguish a proven zombie/exited writer
  from an unsupported live runtime. The original coherence assertions pass.
- SDK runtime catalogs and the shared capability inventory initially omitted the
  new operations. Both inventories were completed, and full parity tests passed.
- Strict lint found local style issues, corrected without suppression.
- Partial patch deliveries and context-pressure withholding were recovered through
  retained results rather than blindly replaying mutation calls.

Compiler-cache growth required several explicitly authorized, manifest-backed
cleanups while the coordinated compiler was idle. Only regenerable incremental
cache was removed. Active/linked older binaries, source, durable user data and
verification evidence were preserved. The candidate build completed as a valid
Mach-O arm64 executable, not merely a successful Cargo exit code.

## Evidence locations and limits

Private evidence root: `~/.jcode/scratch/phase04-wp03-20260913/`.
Key records include `LEDGER.md`, `verify-final-rust.sh`, `activation-preflight.json`,
`activation-verified.json`, `activated-field-results.json`, native fixture scripts
and cache-cleanup manifests. Exact run/artifact hashes are indexed in the private
candidate verification record.

Native fixture results are under `~/.jcode/scratch/wp03-native-kvxmv9ko/`,
`wp03-native-9mgb0dj_/`, `wp03-native-r0awq03o/` and `wp03-native-ra003ev_/`.
Each records the actual binary/version, result, provider requests and cleanup.

This is requirement-mapped focused verification, not a claim that the entire
repository test suite passes. WP-02's historical full-suite failure inventory remains
explicit in `EXECUTION_FAILURE_TRIAGE.md/json`. Full Linux/Windows application
runtime, arbitrary remote-service behavior and prompt/skill quality are not proven
by these tests. Inspection capture loads a coherent Session and shares unchanged
stored blobs; it is not claimed to be constant-time for large histories. The real
558-message outline took about 27 seconds, off the Agent/event-loop ownership path.

WP-04 still owns actual isolated children/relationships. WP-05 owns the task-monitor
and child Context Editor navigation. WP-06 and independent closeout still reconcile
the combined phase. Mirza accepted the disclosed evidence and boundaries above.
The documentation-only accepted-status closeout does not change activated code.
