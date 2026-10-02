# Runtime supervision acceptance ledger

**Candidate implementation evidence, 2026-10-02.** Covers SP-58-C01/WP-09
(R32–R35). Mirza's implementation acceptance, the reviewed real login-service
activation and downstream publication are separate steps recorded in the external
completion report. Native acceptance is macOS arm64 with owned fixtures, isolated
state roots, an isolated LaunchAgent directory and labels, and a scripted localhost
provider. No paid inference, real-daemon crash or real-service experiment was an
acceptance fixture.

## Exercised boundary

Implementation commits from `f1adfcde9` through `05d4d9c6a`, plus a final
service-plan wording correction, on
`mirza/sp58-c01-wp09-runtime-supervision` from `ceb39d99d`. Supervision journeys
and the WP-08 regression ran against an immutable copy of the `05d4d9c6a` selfdev
build (SHA-256 `3714e4df518ec7fdfe262c98206d9e1f740b24cfd920014e655d3e3710758a71`).
Activation identity is recorded in the completion report.

Journeys: `scripts/run_isolated_test.py python3 scripts/test_runtime_supervision.py
--binary <image> --artifact-dir <dir>` (all stages passed, exit 0, every fixture
job unloaded) and the WP-08 regression `scripts/test_runtime_work.py` on the same
image (passed: finish/cancel input, foreground survival through daemon exit,
background Stop, background survival).

## R32: planned restart and reload continue working primaries without replay

| Subcriterion | Evidence | Disposition |
|---|---|---|
| Reload validates the replacement before touching work | Journey `invalid_reload_refused_before_effects`: a failing shared-server candidate is refused by its smoke test; the held turn is not interrupted, no continuation, runtime keeps serving | Verified |
| Reload waits for real quiescence and checkpoints, continues exactly the interrupted turn | Journey `reload_continues_interrupted_turn_once`: same-PID exec, one continuation request carrying the interrupted prompt and reload directive, idle peer makes no request, no recovery item | Verified |
| Failed reload after interruption keeps serving and continues locally | `primary_host::failed_reload_after_interruption_continues_the_turn_locally_once` (real Agent turn, one provider call, record settled, intent retired, no recovery item) | Validated through production owners |
| Reviewed restart reaches a new incarnation without intentional Stop | Journey `restart_new_incarnation_continues_once` (different runtime identity, `desired_stopped=false`, `wait` reports Restarted, one continuation); `shutdown_tests::verified_restart_publishes_a_replacement_exit_without_intentional_stop`; base `verified_restart_keeps_desired_running_and_hands_interrupted_turns_to_continuation` | Verified |
| Only runtime-owned intents wake sessions; legacy intents stay attach-only and yield to an unresolved recovery item | `primary_host::failed_reload_after_interruption_continues_the_turn_locally_once` (legacy intent untouched); `client_state_tests::history_reports_crash_recovery_without_inferring_continuation` | Validated |
| Durable handoff, incarnation-scoped, crash-safe | Base `reload_handoff_receipt_classifies_only_its_own_incarnation`, `failed_continuation_persistence_retains_the_turn_record_for_retry`; `only_planned_transitions_retain_interrupted_turn_records` | Validated |
| Selfdev initiator | Live activation build-reload of this candidate (see completion report) | Recorded at activation |
| Executions left by earlier runtime images never block a restart or Finish | Found while preparing the real handoff (eleven lost-owner rows would have made a reviewed restart `Blocked`). `execution::shutdown::tests::historical_rows_whose_owner_is_gone_neither_run_nor_block_stop` fails without the Stop fix (`Execution control endpoint is no longer owned`) and passes with it; `runtime_startup_settles_lost_owner_rows`; journey `startup_settles_lost_owner_row` (crash during an in-process tool, row settled `interrupted`/`owner_crash` when the replacement claims the namespace, not listed as unfinished work) | Verified |
| Native survivor and child quiescence regressions | WP-08 `test_runtime_work.py` on the candidate image; reload interrupts owned delegations through the existing `await_delegations_for_reload` | Verified (native), child path validated |

## R33: unexpected exit restores availability, inference needs a decision

| Subcriterion | Evidence | Disposition |
|---|---|---|
| SIGKILL leaves selectable items, no inference without a decision | Journey `crash_selected_recovery`: two `unexpected_exit` items, no provider request after restart | Verified |
| Live and lost native owners | Journey `crash_reports_live_native_owner`: a background command survives the crash, is listed with `live_owner: true`, completes once under its own owner | Verified |
| Two-client decision race resolves once | Journey: two concurrent `runtime recover continue` with the same revision, exactly one succeeds, one continuation request with the approved selected-recovery reminder | Verified |
| Automatic wakes deferred; human input supersedes | Journey: deferred background-role input stays `accepted` with no request; a human message is delivered, resolves the peer's item as `superseded_by_input`, the deferred wake follows, a later Continue is rejected; `primary_host::unresolved_crash_recovery_defers_automatic_wakes_until_human_input_supersedes` | Verified |
| Leave stopped | Journey `sigterm_external_signal_leave_stopped` (no request); `primary_host::leave_stopped_needs_no_inference_and_releases_deferred_wakes` | Verified |
| Continue exactly once, replay by request | `primary_host::selected_continue_delivers_one_turn_and_replays_only_by_request`; `shutdown_tests::supervision_status_and_recovery_decisions_use_the_live_coordinator` | Validated |
| SIGTERM is graceful, not intentional Stop | Journey: exit 0, `desired_stopped=false`, one `external_signal` item; `shutdown_tests::external_signal_quiesces_without_desired_stop_and_reviewed_control_cannot_replace_it` | Verified |
| No client inference | History reports `runtime_recovery` and never `was_interrupted`/inferred directives (`client_state_tests::history_reports_crash_recovery_without_inferring_continuation`); TUI notice/no-queue tests | Validated |

## R34: macOS login service

| Subcriterion | Evidence | Disposition |
|---|---|---|
| Reviewed, digest-bound, credential-free plan | `runtime_service` plan tests (including that a private installer `TMPDIR` is not carried, found while reviewing the real plan from a tool shell); journey checks label/definition/program/arguments, no credential variables, carried `HOME`/`JCODE_HOME` | Verified |
| Install beside an unmanaged runtime; takeover by reviewed restart | Journey `service_waits_behind_unmanaged`, `restart_handoff_to_service` (unmanaged exits 0, supervised runtime reports `supervised`, interrupted turn continues once) | Verified |
| No competing daemon; concurrent Start | Journey: a direct `serve` exits nonzero while supervised; two concurrent `runtime start` succeed without a second runtime | Verified |
| Crash relaunch | Journey `service_crash_relaunch` (SIGKILL → launchd relaunch, new PID, serving, not desired-stopped) | Verified |
| Intentional Stop survives login-equivalent bootstrap | Journey `service_desired_stop_and_start` (reviewed Stop not relaunched; `launchctl kickstart` exits cleanly while stopped; explicit Start runs it) | Verified |
| SIGTERM through launchd | Journey `service_sigterm_then_login_start` (graceful exit not relaunched; next login-equivalent start runs) | Verified |
| Uninstall refused while supervised runtime runs | Journey | Verified |
| A service that waited serves its launcher's current binary | Found after the real handoff (the supervised runtime served the image loaded at install). Journey `takeover_serves_current_launcher_target`: the launcher is retargeted while the service waits; after the handoff the same launchd PID runs the new binary (kernel executable path). Red on the pre-fix build | Verified |
| The supervision marker is not inherited by tools | Found because tool subprocesses of the live supervised runtime believed they were supervised. Journey `supervision_marker_not_inherited` (a shell run by the supervised runtime sees no marker); image replacements carry it (live reload stays `supervised`) | Verified |
| A restarted incarnation is never sealed by its predecessor's receipt | Found in an isolated journey: a rejected stale Begin sealed admission after a restart, so SIGTERM was ignored and launchd killed the process. `admission::tests::restarted_incarnation_is_not_sealed_by_its_predecessors_receipt` (red without the fix); journey `stale_begin_does_not_seal_restarted_runtime` | Verified |
| Real activation | Separately reviewed procedure, see completion report | Recorded at activation |

## R35: power follows runtime work

| Subcriterion | Evidence | Disposition |
|---|---|---|
| Detached work holds an assertion; released after | Journey `power_follows_runtime_work`: `caffeinate -i -s` child of the supervised runtime while a turn runs with no client attached, `pmset -g assertions` shows `PreventUserIdleSystemSleep`, released when the turn settles; status reports `active`/`active_work` | Verified |
| User switch | Journey `power_switch_respected`: `prevent_sleep_while_streaming=false` reread live, no assertion during work, status `enabled=false` | Verified |
| Surviving native worker | Journey `survivor_power`: worker-held assertion persists after a KeepSupported Stop exits the runtime, released when the command settles | Verified |
| Historical rows whose owner is gone do not hold an assertion | Found during activation (eleven lost-owner rows kept the assertion held); `execution::shutdown::tests::running_excludes_historical_rows_whose_owner_is_gone`; Stop inventory unchanged | Validated; live check at activation |
| Existing guard behavior | `power_inhibit` tests | Validated |

## Limits

Native service acceptance is macOS arm64; other platforms report the service as
unsupported. Same-user IPC is a trusted-client boundary, not physical-human
attestation. A crash between a selected Continue decision and its input acceptance
is retried at the next start, not exact-once provider execution. Power assertions
cannot keep the machine awake through explicit sleep, shutdown or power loss.
Pre-existing TUI test failures at `ceb39d99d` are recorded in the completion
report and unchanged by this work.
