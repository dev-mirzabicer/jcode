# Reviewed runtime shutdown backend

This is the C01 runtime backend. Unix `Server::run` installs its lifecycle before
ordinary background dispatch and client acceptance. Native authenticated clients
can use `runtime_probe` and `runtime_control` without constructing a primary
Session. The [narrow CLI](../RUNTIME_CONTROL.md) uses this protocol and the same
durable owner for offline inspection. Harness/SDK adapters and final activated-runtime
acceptance are still being integrated in WP-08. This document does not claim those clients,
macOS service supervision, planned restart or crash-inference recovery are ready.

## Ownership and control

`server::shutdown::RuntimeLifecycle` coordinates the existing PrimaryHost,
execution/native worker, legacy background and preparation owners. The private
`RuntimeStopStore` is an intent/receipt journal, not an execution engine or second
transcript. `RuntimeAdmission` serializes new work, input/control boundaries and
final shutdown publication. No client connection drives the operation lifetime.

A review records exact options, runtime incarnation and affected work. Begin uses
that review and a caller-retained request UUID. Retries inspect the original
operation, never repeat effects. Completed work may drop out of a review, but new
or rebound work requires a fresh review. Inspection remains available while
waiting or blocked.

- **FinishCurrent** fences new independent turns and automatic input injection.
  Admitted causal work may finish. A human wait is an ordinary turn boundary,
  not a requirement to finish an entire project. Natural completion has no timeout.
- **Interrupt** signals actual owners and waits for terminal output and Session
  checkpoints. The configured deadline bounds the quiescence attempt, not a
  permission to force termination automatically.
- **Stop** accounts for independent tasks as well as active primaries.
- **KeepSupported** preserves only verified native command workers. Original run
  identity, cwd, root leases, complete output and authenticated control survive.
  Pending native setup must establish its handoff or block preservation. A failed
  handoff never silently becomes cancellation.

Cancellation reopens admission only while the operation is still cancellable.
`ReviewChange` binds a waiting/blocked operation and exact revision, then normal
Begin adopts that new review atomically. The old operation becomes Superseded and
retains its original review and evidence. Already-stopped effects are not undone.
If stopping previously closed cancellation, reviewing Finish for remaining work
cannot reopen that cancellation boundary.

Reload reserves the same admission transition before publishing its Starting
marker. A previously obtained Stop review cannot race through that reservation.
Failure releases the reload reservation and resumes eligible retained new input,
not already-performed turns. This integration preserves the existing reload
implementation; stronger replacement and crash recovery remain separately owned.

## Verified Stop versus Force

`Stopped` requires actual quiescence, complete owner accounting and durable primary
checkpoints. The server then joins its connection/listener scope and removes only
the witnessed main/debug socket inodes. A replacement file or socket is retained
and reported, not deleted using a guessed sibling path. Intentional Stop is checked
before a later server publishes listeners; explicit Start authorization is required.

A deadline produces Blocked with actual remaining work. Explicit Force targets
only verified runtime-owned process controls. If a force attempt cannot establish
complete quiescence or checkpoints, the journal may record **Forced**, with retained
uncertainty and remaining owners, and authorize this runtime process to exit with
code 2. It does not assert successful graceful Stop or rollback external effects.
KeepSupported must complete its preservation barrier before forced process exit
is permitted. Unknown or failed preservation remains blocked even after Force.

A lost reply after durable stopped/forced publication can be reconciled only by
that same live runtime incarnation, without changing the original receipt or
replaying work. A new Start cannot use an old receipt to stop its replacement.

External SIGTERM/temporary-server exits are not this reviewed contract. The Unix
`server stop --force` process-group termination path is retired with an actionable
migration refusal; `server stop` now produces a review. External-signal and supervised
service lifecycle integration remains separately owned, not described as graceful
quiescence by this backend guide.
Do not activate the complete runtime product before its required command path is
available and verified.

## Deferred delivery and namespaces

A stable namespace derives from the canonical runtime socket path, independently
of a transient PID or execution owner ID. The execution store retains its immutable
owner-to-namespace receipt. Native handoff provenance identifies the original
runtime when a worker owns capture after the parent exits.

New accepted input records persist a delivery namespace alongside the original
unchanged envelope. Startup and safe-boundary drains select only their namespace.
Another running namespace cannot acquire a stopped runtime's queue merely because
it shares Session files. Completion notification/wake claims similarly require the
execution's verified original namespace; mismatched claims remain pending.

Legacy unbound input remains inspectable. Explicit human-origin or typed-client
resubmission may bind an unbound original to the receiving runtime without changing
its bytes or UUID. Automatic enumeration and synthetic wake cannot perform that
adoption. A bound input cannot be rerouted by same-ID replay in another runtime.
Existing original-read and cancellation controls permit deliberate cancellation
and a new submission where appropriate. Receipt inspection explains an unavailable
delivery namespace without rewriting source. Committed effects are never replayed.

New optional journal/input fields preserve reading old records, not permission to
fabricate missing ownership evidence. Older binaries that reject these new fields
must not be used to downgrade active control state. Native workers retain execution
schema 21 and the separately negotiated control-transport compatibility described
in [execution storage](EXECUTION_STORAGE.md).

## Evidence and remaining activation

Unit and production-owner tests cover reviewed replacement, cancellation closure,
reload races, namespace routing, terminal acknowledgement recovery, actual native
handoff and incomplete-owner truth. Server fixtures use real socket requests and
listener shutdown without provisional Sessions. Forced process-exit tests run only
in owned subprocesses with a deliberately retained blocking owner. Platform-native
acceptance is macOS arm64, not a claim of other service-platform parity, adversarial
same-user containment or remote-provider cancellation.

The work-package acceptance ledger must still reconcile final CLI/SDK/TUI journeys,
old-worker compatibility, the activated immutable binary and Mirza's implementation
review before claiming complete R30/R31 delivery.
