# Primary input and location controls

This is the C01 backend contract for durable primary input and explicit location
changes. [Reviewed new-context scope](PRIMARY_CONTEXT_SCOPE.md) documents grant
carry and continuation publication. These backends do not expose the later
workspace management TUI, runtime service supervision, or C04/C05's final interface
and corpus.

## Owners and availability

`PrimaryHost` retains admission and delivery independently of client sockets.
The ordinary Agent loop owns inference. `Session` owns conversation history,
input commit receipts, current location and historical initial cwd. The private
workspace catalog owns location-change intents and its derived Session index.
No new scheduler, conversation store or permission authority is introduced.

A dedicated authenticated control connection can send `primary_control_probe`.
Version 1 advertises durable input separately from `location_enabled`. Location
changes require the existing, default-off `features.managed_primary_launch`
rollout gate. Inspection and cancellation of existing requests remain available
when new changes are disabled. They are exercised with isolated fixture configuration until the
required human management surface is available. An unknown or unsupported
capability is not a successful operation.

On the current runtime, ordinary message and soft-input Ack follows durable
intake, which also makes the public MessageAccepted event truthful. Other Ack
events remain transport/control acknowledgements. No Ack proves provider dispatch
or task completion. Typed callers use the explicit receipts below.
Legacy message IDs are scoped to their connection. Callers needing retry across
connections retain a UUID and use `primary_input`.

## Durable input

`primary_input` accepts an envelope containing its UUID, exact target Session,
content, images, delivery choice and applicable original authority/reminder
metadata. It returns `primary_input_receipt`. Same Session/UUID and identical
input returns the retained state. Different content or policy under that UUID
is a conflict. Input is persisted before acceptance is reported.

Delivery choices:

- `safe_boundary`: an idle primary starts a turn. Busy input enters at the
  existing safe boundary without splitting a tool-result batch. Explicit urgent
  input retains the existing tool-skipping boundary, not location-control
  authority.
- `next_turn`: preserve FIFO deferred input until the active turn finishes.
  Deferred entries do not block eligible safe-boundary input for that active turn.
- `context_only`: append input without requesting inference.

The receipt distinguishes `accepted`, `committed`, `failed` and `cancelled`.
`committed` means that the input identity and appended message IDs reached one
Session checkpoint. It does not mean the provider executed exactly once or
that an agent read, understood or completed the input. A later explicit rewind
or prompt-safe rollback can remove conversation messages without authorizing
re-delivery of a previously committed input. The original envelope remains
available independently of that history projection or edit.

`primary_input_inspect` returns receipt metadata. `primary_input_read` explicitly
returns the original envelope and receipt, including retained images and failure
information. Ordinary status views should not fetch those bodies. Failure does
not discard the original. A deliberate new submission uses a new UUID rather
than replaying provider or tool effects under a completed receipt.

Ordinary hosted and process-owned user appends use the same Session commitment
mechanism. Hosted soft input, NotifySession and background/scheduled delivery
use durable primary admission. Enabled relay prompts also use this host, with
stable relay-event identity and their existing queued/waited response behavior.
Idle typed human input uses the existing bounded Startup Context observation
owner. Synthetic notifications and within-turn injection do not become new
observation triggers. Scheduled items and execution-completion wakes
carry stable producer correlation. The dedicated scheduled control connection
negotiates input capability before sending content. Existing notification and
execution owners still decide whether an occurrence should notify or wake.

One retained delivery worker drains each hosted primary. Its final empty check
is synchronized with new admission. A dropped client cannot stop that worker.
On runtime startup, pending inboxes reconcile against Session receipts before
restoration. Only still-uncommitted input is eligible for dispatch. Committed
messages and completed tools are never replayed to repair a lost reply.
Unavailable physical roots or damaged state preserve the envelope for repair.
This is not the later configurable crash-inference recovery policy.

## Public clients

Harness API v1.8 advertises `primary_control_v1`. Both SDKs negotiate the actual
daemon capabilities before dispatch: Rust `submit_primary_input`,
`inspect_primary_input`, `read_primary_input`, `primary_location` and TypeScript
`submitPrimaryInput`, `inspectPrimaryInput`, `readPrimaryInput`, `primaryLocation`.
They preserve UUID/session/operation correlation and reject unsupported or
mismatched replies. Location rejection stays a structured domain result.
These controls do not attach the client or construct an inference Session.

## TUI delivery and recovery

The current TUI negotiates `client_input_version=1` on the existing primary
stream handshake. Before transport, it writes the complete input, images,
queued instruction intent and launch settings to a private per-input journal
under `client-primary-inputs/<endpoint-digest>/<session>/`. A kernel-held client
lease prevents another live client taking over its pending inputs. After client
replacement, abandoned records can be reconciled for the same endpoint/Session.
The journal is not another transcript or workflow store.

Reconnect reuses the original UUID and payload when acceptance is uncertain.
Acknowledged input is inspected instead of being resubmitted. Queue mode sends
complete `next_turn` input to the runtime, while ordinary interleave keeps its
safe-boundary semantics. Offline input retains images too. The UI stall guard
requests runtime state rather than cancelling and replaying accepted work.
A known failed provider attempt can use the existing retry/fallback policy with
a new UUID, preserving the complete original settings and retry budget. This
remains distinct from transport recovery and does not promise exactly-once
provider execution.

Ctrl-Up requests exact pending-input cancellation before restoring the cancelled
text/images for editing. Already committed input cannot be withdrawn. Cancellation
received before the original request creates a non-runnable tombstone. If queued
prose was never prepared, its original typed intent remains retained in the
client journal and the server's cancellation record, not fabricated as rendered
provider text. The generic input-read API returns the accepted envelope.

New explicit startup submissions have a structural `new_startup` marker.
Unidentifiable older in-flight reload snapshots are retained in a `.legacy-*`
file for human review and are not automatically resent. Known unsent current
snapshots remain usable. Historical bytes that an older client never saved,
such as some old text-only queue attachments, cannot be reconstructed. New
journaled submissions preserve their complete payload. Unsupported older
runtimes retain legacy protocol behavior but cannot claim UUID-based recovery.

## Input persistence and recovery

Private state lives under `durable-state/primary-inputs/`. Original envelopes
and metadata are serialized per Session under a kernel-held transaction lease.
The initialized marker distinguishes a new inbox from a missing established
journal. Corrupt or unknown state is an error, not an empty queue or an implicit
restore of an older `.bak` that could resurrect cancelled work.

At commitment, the Agent appends messages and a digest-bound input receipt in
one Session checkpoint. Inbox state is derived from that receipt afterward.
Crash before the checkpoint leaves the original input pending. Crash after it,
before inbox acknowledgement or transport delivery, reconciles to committed
without appending again. An uncertain post-write error is checked against the
actual persisted Session before live authority is adopted or restored.

The existing prompt-safe preflight/provider-rejection owner may roll back a new,
unanswered input before provider output. That same checkpoint marks its input
receipt rolled back, and inspection reports `failed` while retaining the complete
original envelope. Replaying that UUID cannot append it again. Explicit repair
and a new attempt use a new identity. A later budget failure after safe-boundary
injection retains the earlier tool-result history and committed injected input;
it cannot roll the whole working turn back past completed effects.

Blocked-prompt metadata carries the full input UUID in addition to the existing
request/content checks. The primary presentation cursor also identifies its
input, so peer errors and reconnect replay cannot retry another client's pending
message merely because numeric request IDs match. This adds correlation to the
existing startup/context owners, not a new projection or automatic compaction
policy.

Short synchronous inbox transactions serialize admission, inspection and
commitment. No inbox lease is held while awaiting a provider or another async
operation. Session writer ownership remains the existing primary lease.

## Typed location changes

`primary_location` accepts `change`, `inspect` or `cancel`. A change names the
Session, request UUID, expected Session-location and catalog revisions, explicit
placement and absolute cwd. It does not create a clone, infer a project, relocate
files, grant access or select a different model.

The catalog records the intent and reviewed physical witness before effects.
Idle application makes no model call. During a turn, application waits until
all tool results are present, before the next provider request, or the safe
no-tool completion boundary. A failed turn also leaves a runtime-owned idle
reconciliation path. Cancellation is allowed only before commitment.

Application validates current revisions and physical identity, renders the
approved managed notice in the target location, and checks the complete
candidate provider budget. It atomically checkpoints the new `Session.location`,
`working_dir` and a structurally identified notice before adopting live state.
The catalog index/receipt then reconcile from that committed Session. Lost
index or reply acknowledgement cannot append another notice. Missing, replaced,
offline, stale or cancelled targets do not change the effective binding.

Notice rendering uses the existing managed notification resource
`session-location-changed`. It is occurrence-time text, not a system-prompt
refresh or a template read on every tool call. If source or complete-budget
validation blocks the notice, the pending control remains inspectable rather
than silently dropping the notice or partially changing cwd.

Subsequent tool invocations use the new cwd. Already started native/background
work keeps its recorded cwd. Initial environment messages, prior tool results,
Startup Context captures, current system text and active-skill snapshots remain
historical and unchanged. Explicit agent replacement still uses its established
owner and cannot rewrite the original cwd message.

A trusted location control can restore an unavailable primary specifically for
repair without admitting inference at the old cwd. Normal tool-enabled provider
dispatch still requires a verified current binding. Repair never recreates a
removed directory or substitutes the home directory. Exact frozen instructions
are retained while candidate tool-guidance validation uses the chosen new
execution location.

Catalog restore preserves completed operation receipts and marks derived Session
rows unreconciled. Primary publication validation refreshes that index through
its existing revisioned owner from current Session state. Pending operations
invalidated by restore are not permission to repeat a historical move.

## Explicit legacy adoption

`primary_control_probe` separately advertises `legacy_adoption_version=1`.
`primary_location` accepts `adopt_legacy` with the exact Session, request UUID,
expected old working directory (or explicit unknown), catalog revision, placement
and absolute new cwd. Both SDKs negotiate this capability and correlate the
operation and target Session. A normal move cannot implicitly adopt an unbound
Session. Adoption remains gated by `location_enabled`, not merely API availability.

History inspection stays available without adoption. Once managed rollout is
enabled, an unbound primary cannot dispatch inference until the explicit binding
is committed. Adoption reuses the location intent, safe-boundary, complete-budget,
Session checkpoint and derived-index owners. It appends one non-waking notice and
preserves earlier history and existing frozen system, skill and Startup Context
snapshots. The ordinary prerequisite restoration policy still owns genuinely
missing historical snapshots.

The receipt retains `legacy_origin.working_dir`. A missing old cwd stays unknown
there and in the notice. The selected cwd then becomes the first known binding,
not a reconstructed historical path. Stale reviews reject, pending adoption can
be cancelled, and replay after checkpoint/index/reply loss adds no second notice.

A missing managed binding is damage, not evidence of an unrestricted legacy
Session. Launch receipts and contradictory derived-index history reject that
downgrade and require authoritative state repair. The index never supplies
permission by itself. Native effects validate publication in their original
storage namespace without mutating the index or borrowing later ambient state.

See [native write scope](dev/NATIVE_WRITE_SCOPE.md) for enforced effects and the
explicit shell/external-tool limitations. This API does not activate the later
workspace management UI or advertise a new agent administration tool.

## Verification boundary

Mechanism tests use synthetic input and real isolated Session/catalog storage.
The native route is `scripts/verify_primary_input_location.py`, invoked through
`scripts/run_isolated_test.py` with an explicit immutable binary and private
artifact directory. It uses a localhost scripted provider, real native tools,
lost acknowledgement, full-batch movement, original background cwd, restart,
missing-cwd repair and primary context operations. Its result and cleanup files
are evidence only after that exact run passes.

The acceptance ledger must separately record focused tests, failure attempts,
protected-state checks, strict lint, final source and activated runtime,
remaining caller boundaries, native results and Mirza's implementation approval.
No prompt wording snapshot, prose-quality assertion or paid model benchmark is
part of this contract.

Requirement-specific verification and native reproduction are mapped in
[the acceptance guide](dev/PRIMARY_INPUT_LOCATION_ACCEPTANCE.md).
