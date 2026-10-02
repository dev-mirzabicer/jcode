# Runtime supervision backend

This guide covers planned restart and reload continuation, selected recovery
after an unexpected exit, the macOS login service and runtime power inhibition.
It extends the [reviewed shutdown backend](RUNTIME_SHUTDOWN.md) and reuses its
owners: `RuntimeLifecycle`, `RuntimeAdmission`, the durable `RuntimeStopStore`
journal, `PrimaryHost`, the execution store and `PrimaryInputStore`. There is no
second scheduler, queue, transcript or input engine. Human usage is in
[runtime control](../RUNTIME_CONTROL.md); activated evidence is in the
[acceptance ledger](RUNTIME_SUPERVISION_ACCEPTANCE.md).

## Evidence of interrupted work

`runtime_lifecycle::turns::TurnJournal` keeps one record per processing primary
under `runtime-lifecycle/<namespace>/turns/<session>.json`. `PrimaryHost::admit`
writes it (with the runtime incarnation and a turn ID) before the turn can reach
a provider; a failed write fails admission. The record is removed when the turn
settles. Two transitions keep it instead:

- a planned replacement (restart, reload) or external termination sets
  `retain_interrupted_turns(true)` before interrupting, so a turn that ends
  *interrupted* leaves its exact record for the next incarnation;
- a crash, SIGKILL or power loss leaves every record of the dead incarnation.

A naturally completing turn, a human Stop and a reviewed Stop always settle.
Session status, transcript shape, reload markers and client presence are never
evidence; the old History heuristics were removed.

## Startup classification

`RuntimeLifecycle::new` claims the namespace, then, before any admission,
`RuntimeStopOwner::reconcile_interrupted_turns` classifies other incarnations'
records:

| Evidence for the record's incarnation | Classification |
|---|---|
| `reload-handoff.json` naming that incarnation | planned reload |
| a reviewed `Restart` operation of that incarnation in phase `Stopped` | planned restart |
| any `Forced` operation | recovery item, `forced_exit` |
| an `ExternalSignal` operation (not completed as planned) | recovery item, `external_signal` |
| nothing | recovery item, `unexpected_exit` |

The reload receipt is written only after verified quiescence, immediately before
exec, so a crash during quiescence remains a crash. A planned record is passed to
a callback that persists a continuation intent (existing `reload_recovery`
store); the record is removed only after that succeeds. Recovery items are adopted
into `recovery.json` before their records are removed. Each step is repeatable
after a crash in the middle of startup.

The same claim reconciles lost execution owners: `OwnedExecutions::settle_lost_owners`
publishes the established evidence-based interruption receipt
(`ExecutionStore::recover_lost_owner`) for unresolved rows of this namespace whose
earlier owner image is gone. Nothing is replayed or signalled, and a still-live
owner such as a handed-off native worker is left alone. Without this, a row left
by an earlier image would hold a Finish review open forever and block Stop. A row
that cannot be settled stays in the Stop inventory, and `OwnedExecutions::stop`
applies the same receipt rather than controlling an endpoint that cannot answer.

## Planned continuation

Startup (and a failed reload in the still-running runtime) calls
`supervision::deliver_planned_continuations`, which submits each pending intent as
empty-content `NextTurn` input with the intent's continuation reminder, using a
correlated input ID (`planned-continuation`, reload ID, session). The input store
deduplicates repeats; the intent is retired after acceptance. Headless members
keep their existing startup owner. A session that also has an unresolved recovery
item never receives a planned continuation; the stale plan is discarded because
the unexpected exit happened later. An unreadable recovery state retains the plan.
The selfdev initiator keeps its exact reload-context directive only when the
context names this reload's target version; every other interrupted turn receives
the existing interrupted-turn continuation. These intents are marked
runtime-owned. Intents written by earlier versions carry no such evidence: the
runtime never wakes their sessions; History still attaches them so a reattaching
client continues as before, unless the session has an unresolved recovery item.
Clients otherwise never synthesize continuations.

## Reload

`server::reload::await_reload_signal` reserves the reload admission transition,
acknowledges the signal (so the selfdev initiator's tool can return), validates
the replacement binary (existence plus a version smoke test) and only then:

1. interrupts every primary turn with `ReloadQuiescence` and every owned child
   delegation, retaining interrupted records;
2. waits for the host to report no processing turn, finalizes in-process
   background tasks and idle child runtimes;
3. checkpoints Sessions through `checkpoint_runtime` (fenced hosts only);
4. persists continuation intents for the retained records and the handoff receipt;
5. execs.

The whole sequence is bounded by 30 seconds. A deadline or any failure is a Failed
reload: the runtime keeps serving, settles the retained records, releases the
fence and continues the interrupted turns locally. An exec failure after the
sockets were released exits 42; the durable records survive for the next start.
Native foreground commands are handed to the background by their proxy on
`ReloadQuiescence`, as before. A supervised runtime waiting for the namespace's
daemon lock never tries the lock while another live process has a `Starting`
reload marker, so an in-place reload is not mistaken for an exit.

## Restart

`ShutdownOptions.destination = restart` (requires `runtime_supervision_v1`) runs
the ordinary reviewed quiescence (FinishCurrent or Interrupt, Stop or
KeepSupported) without recording intentional Stop: `desired_stopped` follows
`ShutdownOperation::records_intentional_stop`. Admission stays fenced through
Stopping regardless of the desired state. The exit is published with
`restart: true`; after socket cleanup `Server::run` either hands off to an
installed login service (unmanaged runtime: kickstart, then exit 0) or replaces
its image with the shared-server candidate and its original arguments. Failure
exits 43 with durable records intact. `runtime change` preserves the destination.

## External signal

Unix SIGTERM calls `RuntimeLifecycle::begin_external_signal`: one atomic
review+begin under the admission lock with Interrupt, KeepSupported and a 20 second
quiescence deadline (below the service's 45 second exit timeout). The operation
has origin `external_signal`, cannot be replaced by a reviewed change and does not
record intentional Stop. If another transition owns admission the signal is
logged and that transition continues. Interrupted turns become recovery items at
the next start.

## Recovery decisions

`RuntimeRequest::Supervision` returns namespace, incarnation, whether the login
service launched the process, power status and recovery items (unresolved first,
then retained history; at most 200 resolved items are kept). Unresolved items are
enriched with fresh unresolved-execution facts for presentation only, including
whether a command still has a live owner.

`RuntimeRequest::Recover` resolves one item exactly once by revision and request
ID. The same request replays the stored result; another request is rejected.
`Continue` requires a published primary, records `Continued { input }` with a
deterministic input ID, then submits empty-content `NextTurn` input carrying the
approved selected-recovery reminder. If submission fails, the decision stands and
startup resubmits any continued input the store has not accepted. `LeaveStopped`
records the decision and re-kicks ordinary deferred delivery.

While an item is unresolved, the durable input drain defers automatic input for
that session. A human-origin input is selected out of order, supersedes every
unresolved item of the session (`SupersededByInput`) and is delivered; a later
trusted decision cannot inject a stale continuation. Direct client messages and
`resume_all_sessions` use the same store. History carries the oldest unresolved
item as `runtime_recovery` and no longer reports `was_interrupted` or an inferred
`reload_recovery`; the TUI shows the item and the commands to resolve it.

Decisions are live-runtime operations. Offline, `jcode runtime recover` and
status read the journal without changing it.

## macOS login service

`jcode-base::runtime_service` is an availability adapter; the lifecycle journal
remains the authority for desired Stop/Start, quiescence and recovery. One
LaunchAgent per socket namespace, label `dev.jcode.runtime.<16 hex of namespace>`,
definition in `~/Library/LaunchAgents` (override with `JCODE_LAUNCH_AGENTS_DIR`
for isolated tests). The plan is deterministic and digest-bound:
`ProgramArguments` are the stable shared-server launcher plus `serve --socket`;
the environment carries filtered absolute PATH, the socket, the supervised
marker, deferred auth bootstrap and the installer's existing path selectors
(`HOME`, `JCODE_HOME`, `JCODE_RUNTIME_DIR` and the `XDG_*` runtime, config,
data, state and cache directories), so the service resolves the same namespace,
durable state, configuration and credential stores as the installing shell.
`TMPDIR` is not carried: launchd supplies the user's standard temporary
directory, where an ordinary runtime keeps its daemon lock, so an installer with
a private `TMPDIR` cannot point the service at a different lock. No credential is
copied. `RunAtLoad`,
`KeepAlive { SuccessfulExit = false }`, `AbandonProcessGroup`, `ExitTimeOut 45`,
`ThrottleInterval 10`.

A supervised launch waits for the daemon lock instead of failing beside a live
unmanaged runtime, then exits 0 without starting if the journal records an
intentional Stop. Intentional Stop and SIGTERM exits are successful, so launchd
does not relaunch them; crashes and forced exits are unsuccessful and relaunch.
Explicit Start clears desired Stop under the spawn lock, then kickstarts the
registered job and waits for readiness; automatic spawn does the same only when
no intentional Stop is recorded. Neither spawns an unmanaged daemon while a job
is registered. Install never stops a running runtime; uninstall is refused while
the supervised runtime runs. Other platforms report the service as unsupported.

## Power

`supervision::spawn_power_monitor` reconciles one `PowerInhibitor` every five
seconds from `RuntimeLifecycle::active_work()`: admitted primary turns and
preparations, runtime-owned background tasks, and inventoried executions whose
originating runtime or current owner still holds its live lease. A historical
row whose owner is gone stays visible to Stop review but runs nothing, so it does
not keep the machine awake. An observation failure keeps the assertion. Clients, idle sessions and human waits do not count. The command
worker holds its own inhibitor while its command runs (first after five seconds),
so a command preserved across Stop keeps the machine awake until it settles; it
is released when the worker finalizes. Both read
`power.prevent_sleep_while_streaming` on every reconcile; `JCODE_DISABLE_POWER_INHIBIT`
disables both. `PowerStatus.active` reports a held platform assertion
(`caffeinate -i -s` on macOS), not merely the desired state.

## Protocol and compatibility

`runtime_probe` replies with `version: 1` and `supervision: 1`. Harness v1.12
advertises `runtime_supervision_v1`; Rust and TypeScript SDKs refuse restart
reviews, `supervision` and `recover` before sending when either the bridge or the
native runtime lacks it (`RuntimeRequest::requires_supervision`). New fields
default (`destination: stopped`, `origin: reviewed`) and are omitted when default,
so existing stop controls are byte-compatible. The shared correlation matrix
(`runtime_correlation.json`) covers the new requests in both languages.

## Verification

Mechanism tests: `jcode-base` `runtime_lifecycle::tests` (classification,
idempotent reconciliation, external-signal rules), `turns`, `recovery`,
`runtime_service` (deterministic, namespaced, credential-free plan; digest
binding); `jcode-app-core` `server::shutdown_tests` (restart exit, signal,
supervision/recover through the coordinator) and
`server::client_lifecycle::tests::primary_host` (deferral, human supersession,
continue once, leave stopped, retained records through a real Agent turn);
History and TUI notice tests; SDK negotiation tests. Native journeys use
`scripts/test_runtime_supervision.py` through `scripts/run_isolated_test.py` with
an owned namespace and an isolated LaunchAgent directory and label.
