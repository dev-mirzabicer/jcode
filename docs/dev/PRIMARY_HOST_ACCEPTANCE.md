# Primary creation and runtime ownership verification

This guide covers SP-58-C01/WP-03. It describes mechanism evidence, not approval
of the whole workspace program or the later management interface.

## Current contract and staging

Ordinary shared-server turns are runtime-owned. Closing or navigating clients
does not stop a primary or replace its cwd. Explicit Stop still reaches the
actual turn and native work. Local Run/REPL inference remains process-owned.
Temporary server policy remains explicit and separate.

Managed creation uses `features.managed_primary_launch`, default **false**.
The tested public adapters require explicit placement/cwd and a retained request
UUID. They do not enable mandatory legacy adoption, grant enforcement, managed
cloning, checkout removal, login-service supervision or workspace agent tools.
Those remain later C01 packages. The final visual client belongs to C04 and
final instruction corpus to C05.

See [server architecture](../SERVER_ARCHITECTURE.md) for the actual request
shape, ownership, cursor protocol, persistence order and failure boundaries.

## Requirement-to-evidence map

| Requirement | Production owner and evidence |
|---|---|
| R07, explicit placement/cwd and no implicit clone | `PrimaryLauncher`, catalog `prepare_primary_location` and `prepare_launch_filesystem`. Service tests exercise all five placements, missing cwd, explicit empty directories and association. `verify_primary_launch.py` exercises all five through the actual daemon, empty standalone creation, CLI Run, direct REPL, Harness and ACP. `verify_primary_tui.py` exercises actual launch-file TUI startup. |
| R08, complete preparation and publication | Existing Agent instruction/Startup Context/provider owners, Session creation checkpoint, catalog launch journal and exclusive owner lease. Tests cover invalid model/profile/startup, no Session orphan, zero inference during preparation, frozen concrete route, same-request replay, before/after-index interruption, catalog restore and exact snapshots. Native creation replays across a second connection without a second Session. Existing startup and instruction families preserve concrete-cwd specificity and frozen state. |
| R10, runtime-owned admission/completion | `PrimaryHost` admits once, retains actual task/control ownership, settles history before terminal delivery and owns stdin routing. Host tests cover competing owners, reserved/running Stop, idle detached wake and recovery admission races. `verify_primary_host.py` runs an ordinary non-debug daemon for more than 330 clientless seconds with a real native command, reconnects to complete retained output, then stops another actual command from a new attachment. |
| R11, attachment/navigation and bounded delivery | Authoritative target cwd and independent restoration, connection-only cleanup, primary-retained MCP resources, editor lease release, session-bound delivery generations and ordered checkpoints/cursors. The actual TUI types a turn, navigates through the picker while it is still processing, returns to complete output and reconnects at a narrow size. `verify_primary_stream.py` fills a real unread Unix-socket observer, verifies disconnection, then obtains all 264,000 canonical output bytes through negotiated resnapshot with one original provider call. |

Counts are not substitutes for this mapping. An activated run must identify the
exact binary, source and fixture artifacts used. Ordinary-daemon, debug-enabled
UI and in-process recording-provider results are distinct evidence classes.

## Focused mechanism routes

Use coordinated selfdev tests and `scripts/dev_cargo.sh`; the latter isolates
HOME/Jcode/runtime state for tests. Serialize environment-sensitive app-core
families. Useful final filters include:

- Base `workspace::`, `session::tests`, `startup_context::`, `instruction::` and
  `mcp::`.
- App-core `primary::launch::tests`, `primary_mcp_registration_is_retained_`,
  `agent::startup_context::tests`, `server::client_lifecycle::tests`,
  `server::client_session::tests` and `delegation::tests`.
- Complete protocol, Session types, context-core, Harness bridge and Rust SDK.
- TUI `tui::backend::tests`, `context_editor::tests`, the authoritative snapshot
  replay reducer and busy workspace navigation tests.
- TypeScript `npm --prefix sdk/typescript run check`.
- Strict Clippy on changed targets, workspace formatting, full-range diff checks
  and protected-state comparison.

Tests use synthetic content to verify machinery. They do not snapshot or grade
approved prompt/skill prose. Existing disabled memory/Swarm policy remains in
force. A deliberately invoked old enabled-policy fixture is not a reactivation.

## Native routes

Pass an immutable candidate path, never assume the latest Cargo output is the
activated binary. Each script requires an isolated environment and retains
private result/cleanup evidence under its artifact directory:

```sh
python3 scripts/run_isolated_test.py python3 scripts/verify_primary_launch.py \
  --binary /absolute/candidate/jcode --artifact-dir /private/launch-evidence
python3 scripts/run_isolated_test.py python3 scripts/verify_primary_tui.py \
  --binary /absolute/candidate/jcode --artifact-dir /private/tui-evidence
python3 scripts/run_isolated_test.py python3 scripts/verify_primary_stream.py \
  --binary /absolute/candidate/jcode --artifact-dir /private/stream-evidence
python3 scripts/run_isolated_test.py python3 scripts/verify_primary_host.py \
  --binary /absolute/candidate/jcode --evidence-parent /private/host-evidence
```

The scripts use localhost scripted providers, owned directories, sockets and
processes. No paid model traffic or real user checkout is needed. Native UI
checks dismiss fixture onboarding before typing and observe actual picker
readiness. The existing picker omits unused conversations, so its peer fixture
has an explicit context-only message. Debug frames of early-return overlays can
be stale; final ordinary-session frames and explicit target state establish the
observed navigation, not a guessed new frame number.

A reader can overflow again during an active burst. The stream fixture retries
only attachment/snapshot reading, never the original model request. Fixture
cleanup must stop exact owned testers and processes before final artifact writes.
Do not unload or kill a real user runtime for testing.

## Evidence limits

Native acceptance is macOS arm64. Empty-cwd publication uses macOS exclusive
rename and fails explicitly where that primitive is unsupported. Kernel/session
ownership is not an OS sandbox. Arbitrary shell/external effects and hostile
same-user filesystem races are not claimed contained. Trusted IPC is not proof
of a physical human.

Presentation checkpoints are disposable projections of canonical Session
history. Uncommitted stopped provider text is retained for live inspection, not
fabricated into history or promised durable after process loss. Planned reload,
crash continuation, supervision and final shutdown policies retain their later
package ownership. Final acceptance records must separately state source HEAD,
activated implementation, channels/canary and any documentation-only tail.

## Independent-review repair checks

`verify_primary_review.py` exercises detached NotifySession both before any
target attachment and after last-client detach, unknown-target rejection,
managed and legacy two-client Clear, isolated model changes, persisted location
and index, and restart/reattach. Run it with `run_isolated_test.py`, `--binary`
and `--artifact-dir` like the public launch fixture. It creates no real schedule
and calls only a local scripted provider. The TUI fixture also clears through
actual input and returns to the retained source.

Focused regressions include `notify_session_terminal_race_`,
`scheduled_live_delivery_uses_notify_`, `clear_failure_and_concurrent_peer_`,
`handle_clear_session_`, and the all-placements launch service test. The race
fixtures coordinate exact admission/terminal boundaries instead of weakening
assertions with timing retries. Clear's source and replacement are distinct
owners; identity-mismatch checks remain enforced.
