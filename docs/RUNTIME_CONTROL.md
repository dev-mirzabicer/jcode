# Runtime control

On the Unix hosted runtime, `jcode runtime` provides reviewed shutdown controls
without creating an agent Session. Use the same global `--socket PATH` for every
step when controlling a named runtime. These are same-user administrative controls,
not a physical-human attestation or an OS sandbox.

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

Login-service supervision, selected crash-inference recovery and stronger planned
restart controls belong to the subsequent runtime-service work. External SIGTERM
and temporary-server lifecycle policies are not this reviewed Stop command.
[Activated-runtime evidence](dev/RUNTIME_SHUTDOWN_ACCEPTANCE.md) is recorded
separately from implementation approval and later full C01 client acceptance.
See [the backend guide](dev/RUNTIME_SHUTDOWN.md) for ownership, journal, namespace,
failure and verification details.

## Harness and SDK clients

Harness v1.11 advertises `runtime_lifecycle_v1`. Rust `runtime_control` and
TypeScript `runtimeControl` probe the native version before sending control and
validate the returned review/request/operation identities, not just transport IDs.
They require no Session attachment. Use `ensure_runtime: false` / `ensureRuntime:
false` to connect for administration without provisioning a private instance.
Their typed error responses remain domain refusals. Offline inspection and
explicit Start use the CLI above, not an SDK-owned control-state copy. See the
[SDK workflow](../sdk/typescript/README.md#reviewed-runtime-control).
