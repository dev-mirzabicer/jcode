# Shared execution: native candidate verification

Status: partial WP-02 acceptance evidence, not package acceptance or final activation.

## Candidate boundary

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

## Platform verification boundary

Rust standard libraries for `aarch64-unknown-linux-gnu` and
`x86_64-pc-windows-msvc` were installed. `jcode-tool-types`, `jcode-harness-api`, and
`jcode-transport` compile for both targets, including the Windows named-pipe code.

Full protocol/SDK cross checks were attempted but blocked in native `ring` build
prerequisites: missing `aarch64-linux-gnu-gcc` and Windows C runtime headers.
The protocol reaches that dependency through provider-core/reqwest, so it was not
an independent pure-Rust check. These failures are not passing application or
SDK compilation evidence. No native Linux/Windows execution or process-control
parity is claimed. Unsupported storage/process branches remain an explicit final
platform-reconciliation item.

## Still required for WP-02

This file is not the whole requirement matrix. Final configuration cutover,
combined producer/caller/permission/capability review, remaining platform and
force-stop semantics, prerequisite regressions, shared-runtime build-reload and
canary, native TUI input/frames, final diff review, Mirza's candidate approval,
and durable publication/closeout remain separate completion conditions.
