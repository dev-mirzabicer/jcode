# Runtime control

On the Unix hosted runtime, `jcode runtime` provides reviewed shutdown controls
without creating an agent Session. Use the same global `--socket PATH` for every
step when controlling a named runtime. These are same-user administrative controls,
not a physical-human attestation or an OS sandbox.

Inside an attached TUI, `/runtime` opens the same reviewed controls in the
[workspace management mode](WORKSPACE_MANAGEMENT.md): status, Stop and Restart
reviews, waiting Cancel, option change, Retry, typed Force, interrupted-turn
decisions and, while stopped, an explicit Start that runs `jcode runtime start`
for that socket.

## Inspect and start

```sh
jcode runtime status
jcode runtime start
```

Status never starts a process. It distinguishes a live, namespace-verified reply
from a durable offline receipt and reports whether the coordinator's kernel lease
is still owned. An unavailable socket or a PID alone is not a stopped-work proof.

Start is explicit authorization to make an intentionally stopped runtime available.
The spawn lock serializes starts, and the client verifies the resulting namespace
and capability. Start cannot cancel a live shutdown. It does not replay completed
turns or tool effects. Retained, eligible new inputs can be delivered normally.
A failed start remains a reported failure, not an optimistic Running result.

Ordinary TUI, delegation, ACP and keepalive auto-spawn paths consult intentional
Stop under the same spawn lock. They cannot undo it to deliver a notification.
When the namespace has an installed login service (macOS), Start and automatic
spawn ask that service to run instead of spawning a competing daemon.

Status also reports, when the runtime supports `runtime_supervision_v1`, whether
the login service launched it, the power assertion state and any turns awaiting a
recovery decision, plus the login service registration on macOS.

## Review and confirm Stop

```sh
jcode runtime stop
jcode runtime stop --strategy interrupt --tasks keep-supported --json
```

Stop produces a review, not shutdown. The default is `finish-current` with
independent tasks `stop`. `--strategy interrupt` requests actual cancellation of
owned work. `--tasks keep-supported` requests verified native-command survival.
Other in-process, child-inference, browser and MCP operations are not advertised
as independently surviving. Use `/tasks` to inspect listed run IDs and their input
and retained output when needed.

The review displays the runtime identity, exact options and affected work, and
provides a request UUID for confirmation:

```sh
jcode runtime confirm REVIEW_UUID --request REQUEST_UUID
```

Replace those UUIDs with the returned values. The human-readable review prints a
shell-quoted command for the selected socket. Retain both UUIDs if the reply is
lost. Repeating that exact confirmation recovers the original operation, including
a completed offline receipt; it does not begin a second shutdown. A changed owner
or newly affected work rejects stale review rather than expanding its authority.

Confirmation is acceptance, not completion. Use the returned operation ID:

```sh
jcode runtime inspect OPERATION_UUID
jcode runtime wait OPERATION_UUID --timeout-seconds 60
```

Wait never cancels or forces work. FinishCurrent has no natural-completion
deadline; `--quiescence-seconds` on a review applies only after entering Stopping.
A wait timeout is a client wait result, not evidence the runtime stopped. Verified
Stopped is distinct from still-owned coordinator cleanup and from Forced.

Native commands admitted before their execution record is published remain
pending work. Finish/Keep waits through that preparation without cancelling or
repeating it. An observation failure is reported while waiting remains cancellable;
the runtime retries observation automatically. If the diagnostic itself cannot be
persisted, live inspection reports the storage failure rather than healthy waiting.
Repairing observation/storage does not replay a command or automatically escalate Stop.

## Restart

```sh
jcode runtime restart
jcode --socket PATH runtime confirm REVIEW_UUID --request REQUEST_UUID
jcode runtime wait OPERATION_UUID --timeout-seconds 120
```

Restart uses the same review, options, confirmation and quiescence as Stop, then
replaces the runtime instead of leaving it stopped. It never records an
intentional Stop. Primary turns it interrupts continue once in the new runtime
through ordinary durable input; idle and human-waiting sessions stay idle; no
completed tool effect is replayed. Under the login service the process image is
replaced in place. An unmanaged runtime with an installed service hands the
namespace to that service; otherwise it replaces itself in place. Wait reports
success only when a new runtime incarnation answers. `runtime change` keeps a
restart a restart.

Selfdev and `server reload` reloads use the same principle: before exec the
runtime validates the replacement binary, interrupts every primary turn at a
reload boundary, waits for actual terminal persistence, child/background
quiescence and Session checkpoints, and records a durable handoff. A deadline or
failure keeps the current runtime serving and continues the interrupted turns
there; it is never permission to exec with unsettled work.

## Recover after an unexpected exit

If the runtime ends without a verified Stop, restart or reload (crash, SIGKILL,
power loss, forced exit, or termination by the system), the next start does not
resume interrupted turns on its own. Each interrupted primary turn becomes a
recovery item:

```sh
jcode runtime recover
jcode runtime recover continue RECOVERY_UUID
jcode runtime recover leave RECOVERY_UUID
```

Listing shows the cause, the session and any unfinished execution of that session
(including commands still running under their own owner, which must not be
repeated). Continue delivers one continuation turn through ordinary durable input
and tells the agent to check retained results before repeating any effect. Leave
keeps the turn stopped. A decision binds to the item revision and is recorded
exactly once; retry an uncertain reply with `--request UUID` from the first
attempt. Sending a new message to the session also resolves its item: the message
is delivered normally and no stale continuation follows it. While an item is
unresolved, automatic wakes for that session (background completions, schedules)
stay durable but are not delivered. Clients show the pending item; they never
infer a continuation from the transcript.

External SIGTERM (logout, service unload, `kill`) enters a graceful Interrupt that
preserves supported native commands, bounded by 20 seconds. It does not record an
intentional Stop, so the next login starts the runtime normally; its interrupted
turns become recovery items.

## Login service (macOS)

```sh
jcode runtime service status
jcode runtime service install
jcode runtime service install --confirm DIGEST
jcode runtime service uninstall
```

Install first prints the exact plan without writing anything: the namespaced
LaunchAgent label and definition path, the stable launcher
(`~/.jcode/builds/shared-server/jcode serve --socket SOCKET`), its environment
(the installer's existing absolute PATH entries, socket, and the home, state and
configuration path variables that locate its stores; no credentials), log file, restart throttle and exit timeout. Installing writes and
loads exactly the confirmed plan. The service starts the runtime at login unless
it was intentionally stopped, restarts it after an unexpected exit (exit-based, so
an intentional Stop is never undone), and leaves surviving native task processes
alone. An already running unmanaged runtime keeps serving; the service waits and
takes over when it exits, for example after a reviewed restart. Uninstall is
refused while the supervised runtime runs; stop it first. Other platforms report
login-service supervision as unsupported.

`status` reports whether the service is installed and loaded and its process.
Whether the installed definition matches the current plan is reported only when
a plan is compared (`install` does this before writing); plain status leaves it
unknown rather than guessing.

## Power

With `power.prevent_sleep_while_streaming` enabled (the default), the runtime
holds a system-sleep assertion while it owns active work: primary turns,
running executions, preparations and background tasks. Attached clients, idle
sessions, human waits and historical execution records whose owner is gone do not
count. A native command that runs longer than a few seconds
holds its own assertion, so a command that survives a runtime Stop still keeps the
machine awake until it settles. The switch is read on every reconcile.
`JCODE_DISABLE_POWER_INHIBIT` disables it. Explicit sleep, shutdown and power loss
are not prevented.

## Cancel, change, retry and escalate

Inspection returns the current operation revision. Controls bind to it:

```sh
jcode runtime cancel OPERATION_UUID --revision REVISION
jcode runtime change OPERATION_UUID --revision REVISION --strategy interrupt --tasks stop
jcode runtime retry OPERATION_UUID --revision REVISION
jcode runtime force OPERATION_UUID --revision REVISION
```

- Cancel works only while cancellation remains open in the waiting phase.
- Change produces another review. Confirm it separately. The prior operation and
  its original evidence remain inspectable as Superseded.
- Retry reattempts quiescence after a blocker is resolved, not the underlying work.
- Force is explicit escalation against the reviewed runtime's proven owners. It
  never silently substitutes cancellation for a failed KeepSupported handoff.
  If complete quiescence/checkpoints cannot be established, an explicit forced
  process exit retains a **Forced** receipt with remaining work and uncertainty.
  It is not graceful Stopped. Wait prints that receipt and exits nonzero.

A changed revision requires inspection, not blind retries. Previously stopped
work is not rolled back. Switching back to Finish cannot reopen cancellation
after irreversible stopping began. Filesystem and external effects already
performed are not undone by Stop or Force.

## JSON, migration and limits

Every command accepts `--json`. Reports contain the selected socket,
`live_response`, nullable `coordinator_owned`, a typed `response`, optional
`confirm_request`, and any offline/transport diagnostic. A missing IPC directory
is reported without creating it. Live control verifies `runtime_lifecycle_v1` and
the actual socket-derived namespace before sending a mutation.

On Unix, `server start` delegates to explicit runtime Start and `server stop`
creates the default review. The old unbounded `server stop --force` is refused
without signalling a process. Use exact operation/revision Force instead. Existing
non-Unix manual server controls remain separate and do not claim managed native
survival; `runtime` reports unsupported there.

A successfully preserved native worker retains its original execution ID, cwd,
root-use leases, control and full output. Its completion does not auto-start an
intentionally stopped runtime. Inspection and result reading after explicit Start
do not repeat the command. Foreign-runtime and legacy-unbound delivery provenance
remain explicit rather than inferred from matching Session IDs or path strings.

Temporary-server lifecycle policies are separate from these controls.
[Activated-runtime evidence](dev/RUNTIME_SHUTDOWN_ACCEPTANCE.md) and
[supervision evidence](dev/RUNTIME_SUPERVISION_ACCEPTANCE.md) are recorded
separately from implementation approval and later full C01 client acceptance.
See the [shutdown](dev/RUNTIME_SHUTDOWN.md) and
[supervision](dev/RUNTIME_SUPERVISION.md) backend guides for ownership, journal,
namespace, failure and verification details.

## Harness and SDK clients

Harness v1.12 advertises `runtime_lifecycle_v1` and `runtime_supervision_v1`.
Restart reviews, `supervision` status and `recover` decisions require the latter
and are refused before sending by clients connected to an older runtime. Rust
`runtime_control` and
TypeScript `runtimeControl` probe the native version before sending control and
validate the returned review/request/operation identities, not just transport IDs.
They require no Session attachment. Use `ensure_runtime: false` / `ensureRuntime:
false` to connect for administration without provisioning a private instance.
Their typed error responses remain domain refusals. Offline inspection and
explicit Start use the CLI above, not an SDK-owned control-state copy. See the
[SDK workflow](../sdk/typescript/README.md#reviewed-runtime-control).
