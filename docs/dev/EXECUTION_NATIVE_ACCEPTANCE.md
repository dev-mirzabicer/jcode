# Shared execution: native candidate verification

## Activated WP-02 candidate, 2026-09-13

Shared/current/running identity is `3485be42d-dirty-39bb6a43a849`, version
`v0.75.308-dev`, with a passed canary and `SocketReady` reload state. Mirza accepted
the candidate on 2026-09-13 at 14:30:30 UTC; exact candidate `6ad23e42b` was published
to local and downstream main at 14:32:34 UTC. Live configuration and all five
protected unrelated file hashes match the pre-activation record.

All ten earlier native CLI/API/PTY cases passed again against this activated
binary with unchanged assertions. An eleventh case exercised the shared output
guard withholding a 1,000,015-byte command result, an explicitly initiated
reasoning-suppression draft/apply transaction through the existing Context Editor
service, and a subsequent real read of the retained tail. The next provider payload
excluded the original synthetic reasoning while retaining its provider-required
blank placeholder. Exactly four localhost requests and one append-only command
effect occurred. No live conversation or context architecture was changed.
This uses the actual manual-operation backend, not physical mouse input or a claim
that compaction was economically required by the single-output size guard.

`post-activation-3485be42d-final.json` in the private WP-02 evidence root records all twelve
cases, exact roots/log hashes, and released runtime leases. Native test clients at
80x24 and 60x24 use the activated binary, have later idle frames after Escape, and
leave no registered testers. Configuration/current/shared/canary were independently
checked in the actual running session. No hosted model was used.

A separate standalone `repl` process also completed the exact retained-output/tail
workflow with three fixture provider requests, one original command effect, and
CLI-owned execution identity. It is not inferred from JSON/NDJSON Run coverage.
Post-activation export tests passed for tool input/result and replay redaction,
exhaustive context-export redaction without source mutation, and active-profile
structural/export integrity (four tests total).

Failed attempts remain evidence. ENOSPC during `rust-objcopy` left a 3 KB
non-executable artifact even though Cargo reported success, and coordinated
validation skipped activation. An initial retry copied the same damaged cached
artifact. Exact inactive copies were quarantined, compiler headroom restored, and
a fresh validated build/reload succeeded. No chmod bypass, security weakening or
force publication was used.

The context fixture first rejected its own command-generation substitution before
execution, then supplied a missing Subscribe workspace, then corrected its expected
apply response to `context_transaction_applied`. Another assertion mistook the
existing provider-required blank reasoning field for meaningful original reasoning.
The final check requires meaningful reasoning to be absent and preserves all
transaction/tail/effect assertions. These failed fixture attempts are not passing
checks and did not justify changing protected context-control behavior.

## Historical isolated candidate boundary

The coordinated TUI build at source `b5fe80d99` produced
`b5fe80d99-dirty-f086431c892b` (`jcode v0.75.297-dev`). These checks executed that
immutable candidate binary, not a linked copy of a function. The shared production
session remained on the accepted WP-01 runtime. Current-channel installation from
`selfdev build` is distinct from activating the shared server or passing its canary.

Tests used private state, working directories, sockets, and a deterministic
localhost OpenAI-compatible HTTP provider with no authentication. No hosted model
inference, Swarm worker, delegated implementation agent, or user application was
used. Later fixture revisions explicitly disabled telemetry. All eight final fixture
runtime leases were checked released after cleanup.

## Actual workflows passed

| Workflow | Observed acceptance |
|---|---|
| Standalone `jcode run --json` | A native command appends one effect marker and emits 80,000 characters plus a tail. The first tool delivery is clipped, the next real read retrieves the complete tail, and stdout is one valid JSON report. The command executes once; the read has no duplicate output bundle. Durable parent runtime PID equals the CLI process. |
| Standalone `jcode run --ndjson` | The same retention/read/effect checks pass with every emitted stdout line decoding as JSON. Durable parent ownership is the CLI process, not a daemon. |
| Native daemon plus Harness API | An attached Session runs the same three-request fixture. Execution list/inspect and nine `read_part` pages retrieve exact raw stdout. Stop on the completed run is not accepted as new cancellation. Execution ownership resolves to the explicitly launched daemon. |
| Foreground Stop during a busy daemon turn | Inspect/read/Stop reply while the real Agent is waiting on the command. Stop reaches terminal `Cancelled`, preserves the captured prefix and earlier effect, and prevents the later effect. The provider receives one failed tool result and finishes without replaying the command. |
| Background work and client reconnection | The parent turn finishes while the command remains running. Closing and reattaching the client does not stop it. Explicit Stop subsequently cancels the same run with prefix/effect preservation. |
| Background work across daemon/bridge restart | The native command retains its original identity and original-parent provenance while both daemon and bridge are replaced. A reattached client sees the same running execution and can stop it. The append-only effect counter remains one. |
| Parent cancellation of foreground work | Session Cancel stops the actual foreground command, preserves its prefix/effect, and prevents a second provider request or command replay. |
| Parent cancellation while waiting on background work | Cancel ends the foreground `bg wait` execution, but the explicitly backgrounded command remains running. It is stopped only by a later explicit execution Stop. Both terminal records and the append-only effect counter are verified. |

The final cancellation fixtures use an append-only effect marker. On any assertion
failure, a separate owned fixture gate releases its synthetic command before
process cleanup; that mechanism is not treated as proof of successful cancellation.
The passing checks require terminal cancellation and preserved output before
cleanup runs.

## Findings from failed fixture attempts

Keep these attempts separate from passing evidence:

- An NDJSON probe initially assumed `--socket` made Run daemon-backed. Current
  `src/cli/dispatch.rs` calls `run_single_message_command`, which constructs a local
  Agent. The corrected fixture checks the actual local PID. Repository `AGENTS.md`
  had the same obsolete claim and was corrected from this source/runtime evidence.
- The first native API probe expected `SessionInfo.id`; the stable API uses
  `session_id`. No provider call occurred before this fixture failure.
- The next API probe waited for a correlated reply to ordinary `send_message`.
  That operation is a notification, with `message_accepted`/`turn_done` events.
  The completed operation was not resent. A fresh isolated fixture used the actual
  event contract.
- A subsequent ownership assertion assumed every Bash run had a worker-handoff
  row. An stdin-capable daemon run correctly uses the in-process captured driver.
  The final check resolves either actual run ownership or its recorded handoff
  parent, rather than requiring one implementation route.

These corrections did not weaken effect-count, complete-output, cancellation,
state, or owner-identity assertions. Fixture attempts and original scripts remain
in the private WP-02 evidence directory, including `native-candidate-b5fe80d99-extended.json`
with exact roots, script hashes, result records and released-lease checks.

## Native TUI keyboard and frames

Candidate PTY testers at 80×24 and 60×24 both passed actual Escape input routing.
Each ran the owned command once, retained its prefix and earlier effect, then
reached durable `Cancelled` without another provider request. Frame capture was
enabled before dispatch; distinct frames showed `RunningTool("bash")` followed by
`Idle`, with empty composer input and no reported layout anomalies at either width.
The narrow terminal's terminal frame also showed the retained cancellation receipt.
These checks use the native tester key-event router, not a live desktop keystroke.
Tester registrations were cleared and all fixture runtime leases were released.

Failed setup attempts are retained: the first used the main socket instead of its
dedicated debug sibling; the second submitted before attachment/onboarding had
finished; an intermediate pass proved cancellation but sampled a stale frame.
Final fixtures use the dedicated endpoint, dismiss only their own onboarding,
wait for the actual server version, and require a later non-processing frame.
No live user configuration or desktop input was changed.

## Platform verification boundary

Rust standard libraries for `aarch64-unknown-linux-gnu` and
`x86_64-pc-windows-msvc` were installed. `jcode-tool-types`, `jcode-harness-api`, and
`jcode-transport` compile for both targets, including the Windows named-pipe code.

Full protocol/SDK cross checks were attempted but blocked in native `ring` build
prerequisites: missing `aarch64-linux-gnu-gcc` and Windows C runtime headers.
The protocol reaches that dependency through provider-core/reqwest, so it was not
an independent pure-Rust check. These failures are not passing application or
SDK compilation evidence. No native Linux/Windows execution or process-control
parity is claimed. The explicit unsupported storage/process boundaries were
disclosed in the candidate Mirza accepted; approval does not supply missing tests.

## Accepted evidence boundary

The combined requirement mapping is in EXECUTION_ACCEPTANCE.md. Configuration
cutover, coordinated shared activation/canary, post-activation native checks,
protected-state and diff review, and Mirza's approval were completed. Historical
fixture failures and cross-platform limitations remain evidence, not hidden green
counts. Routine retention/snapshot policy, isolated children and the new task
monitor remain owned by the next Phase 4 packages.
