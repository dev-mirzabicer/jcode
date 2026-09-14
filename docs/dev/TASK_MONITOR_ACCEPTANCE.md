# Task monitor verification

This is the maintainer verification map for Phase 4 WP-05. Work-package acceptance
is recorded separately after human review. Passing one layer does not imply that
all native workflows or platforms passed.

## Ownership

- `jcode-base::execution::task_monitor` derives bounded metadata pages and verified
  text windows. It never loads payloads for a list or reexecutes a producer.
- Child admission and idle context operations share a short kernel-owned control
  lease. The lease is not held for the human editor lifetime or curator inference.
- `server::context_control::dispatch_editor_request` is the common primary/child
  adapter over the existing context transaction service.
- `server::child_context` restores exact child execution identity and routes only
  human context operations. Local TUI child controls use the compatible shared host.
- `tui::task_monitor` owns presentation, selection and unsent UI intent. `task_ui`
  routes asynchronous local/remote operations and keeps child context state separate
  from primary pressure/transaction state.
- Force stop uses version 2 of the authenticated runtime control protocol. Basic
  controls still use version 1 when the actual owner advertises that version. The
  SDK boundary separately requires `execution_force_stop_v1`.

No context projection, curator policy, parent-child conversation store, scheduler
or archive deletion engine was replaced.

## Requirement-to-check matrix

| Requirement | Production-path checks |
|---|---|
| A34 Active/Completed and scope | Base structural child/batch scope test, current/all native PTY key and SGR mouse actions |
| A34 stable selection | Reducer regression and native command Stop retaining the selected cancelled run |
| A34 full/live output | Verified UTF-8 window reassembly, late-response/pause tests, actual native stream follow/pause and retained prefix |
| A34 Stop/background/force | Existing actual execution owner regressions and native command process observation, original run ID, durable terminal state, preserved effects, shared server surviving |
| A34 storage UI | Modal exact-review test, explicit real-volume fixture setup, physical review and one confirmation against original cleanup transactions |
| A35 responsive layout | Rendered buffers at 140×32, 80×24, 60×24, 48×12 and 47×11, with exact action-hit rectangles and native PTY resize/frame dimensions |
| A35 input ownership | Physical local/remote Enter and key routers, paste preservation, action-menu mouse parity, minimum-size control suppression |
| A35 bounded work | 20,000-record metadata fixture with unavailable payloads, bounded metadata pages, one body read in flight across rapid selection generations, independent controls |
| A36 child targeting | Real child snapshot/curator/review/apply/undo test, separate primary/child protocol signatures, native child title and 60-column frame |
| A36 race handling | Idle/admission lease exclusion, actual busy-child rejection, follow-up making an old draft stale, no parent provider call or source/history mutation |
| A36 current directives | Actual child permission/preset IDs rejected through the existing range preview owner |
| A36 no takeover/restart | Non-context child requests rejected, native inference counters around open/apply/undo/close, original parent continuing deliberately |
| Reconnect | Fresh capability handshake, stable selected identity, no resend of mutations or inference after private server restart |
| Compatibility | Rust and TypeScript pre-transport force-capability negatives, legacy runtime-owner control test, internal probe before child targeting |
| Protected prerequisites | Existing primary context handler suite and context-core regressions, unchanged active configuration/instruction hashes, explicit unrelated-path verification |

## Repeatable commands

All Rust tests run through `selfdev test` or `scripts/dev_cargo.sh test`. Do not
execute current-source migrations against the live Jcode home. See
[`../../scripts/TEST_STATE_ISOLATION.md`](../../scripts/TEST_STATE_ISOLATION.md).

```sh
bash scripts/dev_cargo.sh test --profile selfdev -p jcode-base --lib execution::task_monitor::tests -- --test-threads=1
bash scripts/dev_cargo.sh test --profile selfdev -p jcode-base --lib execution::control_transport::tests -- --test-threads=1
bash scripts/dev_cargo.sh test --profile selfdev -p jcode-tui --lib task_monitor -- --test-threads=1
bash scripts/dev_cargo.sh test --profile selfdev -p jcode-app-core --lib human_child_context -- --test-threads=1
bash scripts/dev_cargo.sh test --profile selfdev -p jcode-app-core --lib server::context_control::tests -- --test-threads=1
bash scripts/dev_cargo.sh test --profile selfdev -p jcode-app-core --lib execution::runtime::tests -- --test-threads=1
bash scripts/dev_cargo.sh test --profile selfdev -p jcode-sdk --test client_behavior execution_force_stop -- --test-threads=1
```

The native script requires an already-built TUI/CLI binary and a separately
identified evidence directory. It owns a private HOME/Jcode/runtime/socket namespace,
a localhost recording provider, and a real PTY. It sends actual terminal keyboard,
mouse and resize input. Debug frame capture observes rendered cells rather than
substituting a render-only fixture.

```sh
python3 scripts/run_isolated_test.py python3 -B scripts/verify_task_monitor.py \
  --binary /absolute/path/to/candidate/jcode \
  --evidence-parent /absolute/path/to/private/evidence \
  --fixture-test-binary /absolute/path/to/current/jcode_base-test-binary
```

The optional test-binary argument enables real reviewed deletion, rather than only
an empty review. Its ignored setup entry point requires an explicit private marker
and unique `jcode-execution-fixture-wp05-*` directory on the verified Active volume.
It prepares a tiny completed cold-archived output and an affected snapshot. The
production TUI then performs review and confirmation. No live user output is aged,
moved or deleted. The fixture's metadata is separate from the live execution index.
The existing archive protection boundary remains unchanged.

Inspect every native `results.json`, exact frames, request counts, process teardown
and failure artifact. A failed attempt is not overwritten into passing evidence.
The native script does not assess prompt quality or make hosted-model calls.

## Evidence boundaries

macOS arm64 is the real native target. Portable type compilation does not establish
Linux/Windows native UI, process or archive parity. Force stop is offered only for
an owner and native process with the required recorded capability/identity. External
service-side compensation and acknowledgement remain outside this feature.

The phase's earlier failed aggregate-test inventory remains in
[`EXECUTION_FAILURE_TRIAGE.md`](EXECUTION_FAILURE_TRIAGE.md). Focused passing tests
must not be represented as a passing complete repository suite.

## Candidate verification observed on 2026-09-14

The private native run `monitor-cc4or0hh` passed actual PTY keys, SGR mouse,
all five monitor sizes, the 60-column child editor, live follow/pause, owned
Stop/background/force, child curator review/apply/undo, unattached child control
without chat allocation, exact Active cleanup with affected-snapshot disclosure,
and same-session reconnect without inference or mutation replay. Its owned cleanup
report is empty, and recorded frames reported no rendering anomalies. Later local
active-loop dispatch wiring passed the actual local input/worker regression.
Final activated-binary verification is recorded separately in the work-package
candidate/progress report, not inferred from this private build.

Focused observed groups include 12 monitor/input/frame tests, five task storage
checks plus the separately invoked native archive fixture, two real hosted child
context tests, eight preserved primary context-handler tests, ten real execution
control tests, 58 context-core tests, a Rust SDK force-capability test and all 51
TypeScript tests. These counts overlap with earlier runs and are not a fabricated
unique-test aggregate. Strict affected library/test Clippy and portable tool/API
checks for Linux arm64 and Windows x64 passed at their recorded source boundaries.

Native acceptance exposed and drove two in-scope corrections: promotion had changed
the background flag without releasing the original waiter, and the monitor had
reset capability/correlation only on History rather than the actual reconnect
boundary. The production owners were repaired and the full native workflow rerun.
Failed probe assumptions, compiler/lint attempts and cleanup reports remain in the
private evidence root. No live user output, instruction source, archive setting or
protected context architecture was changed by a test.

## Human-review UI refinement

Mirza's 2026-09-14 review praised the functional candidate and requested UI/UX
refinement, specifically that original tool input was not discoverable. This is
continued candidate work, not accepted-WP publication.

The refinement makes Input, Output and Info explicit visible tabs. Input starts
at the beginning and displays original typed arguments, including real multiline
string content, with raw receipt JSON available separately. Large partial receipts
remain exact paged JSON rather than being parsed as incomplete argument objects.
Long IDs and capture metadata move to Info; aligned list columns, palette-based
state colors, monochrome selection cues and contextual footer actions improve
readability without another execution or context owner.

Five additional mechanism/render tests cover input-first behavior, exact typed
argument preservation, mouse tab activation, partial input paging, Unicode cell
clipping and monochrome cues. The full native script also reads actual command
arguments at 140×32, 80×24, 60×24 and 48×12, switches raw receipt/Info/Output, and
checks that these inspections do not execute a producer or invoke inference.
The previous live-tail sentinel assertion assumed the first output line would
remain visible. The native check now verifies live tail and then explicitly uses
Home to verify the retained first line, preserving both claims without a timing
assumption.
