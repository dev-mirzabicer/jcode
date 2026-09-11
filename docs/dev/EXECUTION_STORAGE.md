# Execution storage internals

This describes the shared storage/read owner in `jcode-base::execution`. The
Phase 4 WP-02 rollout is still being integrated. The native Registry now uses
this owner, including batch members and ordinary local/shared callers of
that Registry. Producer-specific truncation, provider/SDK-supplied results and
complete detached-command integration remain rollout work. Library and Registry
tests do not establish those remaining paths.

`jcode-app-core::execution` supervises actual tool tasks independently of the
caller waiting for a reply. Scoped replay attaches to existing work or a retained
terminal result without invoking another producer. Working directory participates
in input-conflict checks. Cancelling the original foreground wait stops its work.
Explicit promotion changes waiting ownership without changing invocation identity.
The existing background manager delegates Stop to the actual supervised owner,
not merely the delivery wrapper. Non-owning waits never cancel background work.

The Registry freezes a matching tool definition, input mapping and producer for
the session. Framework options use an unused flat field where safe. Colliding or
open-ended external schemas preserve their original input under `arguments`.
Legacy framework `accept_large_output=true` is rejected before effects. After
retention, character presentation and context withholding are separate delivery
steps. Withholding preserves saved references and does not advance source-read
positions. Batch preserves each member's identity, metadata, media and retained
reference without another per-member byte cap.

## Ownership

The higher execution coordinator owns one complete invocation transaction.
Models, clients and SDK callers must not coordinate prepare/start/capture/seal
stages themselves. `Session` remains the authoritative conversation. Execution
metadata is a receipt/index, not a replacement transcript.

`ExecutionStore` keeps local SQLite metadata with WAL, full synchronization,
foreign keys, private files, short transactions and indexed metadata pages.
Invocation identity includes session, assistant-message identity and the entire
nested call path. An existing scope with different input is a conflict. An
existing invocation is not permission to repeat its producer.

`Capture` owns the streaming sink and the lease on its bundle. `ToolContext`
carries the lower-layer `OutputCapture` interface, avoiding an upward dependency
from tool-core. Inline output, an already captured result and a source-read page
are structurally distinct. Streaming producers return their own reference rather
than duplicating their body through the inline adapter.

## Retention and publication

A bundle contains canonical readable UTF-8, separate original text/stdout/stderr
bytes, observed drain-order events, received media parts and a manifest. Split
UTF-8 scalars remain buffered until complete. Invalid original bytes remain in
raw parts, with replacement-character rendering identified in stream metadata.
Observed drain order is not a claim about producer-time ordering.

Writes are bounded and apply backpressure. Successful retention requires data,
new directory entries and the committed-prefix metadata to be persisted. A
failed writer preserves its existing files. JSON serialization failure must not
cause a buffered writer's destructor to retry an already-retained prefix.

Sealing freezes one result/outcome. A failed terminal metadata write can retry
publication without capturing the body again or accepting a different result.
The durable manifest records terminal outcome and the canonical-text digest.
`recover_terminal_output` validates a sealed result under the output lease. It
can recover publication after owner loss, but cannot invent success from a
missing/legacy manifest or repeat an operation whose effects are uncertain.

Capture failure overrides a successful producer exit. A local failure receipt
identifies the preserved prefix/bundle even when archive storage has failed.
Failed terminal-receipt persistence remains an error, not reported success.

## Location and recovery

Metadata and operation journals remain local. The public output alias is stable;
physical data can be local or on a configured verified archive. The configuration
lives under `output.storage`, with local/archive reserve bytes and an optional
archive mount, volume UUID and relative dedicated directory. Reserves default
to 1 GiB each. No archive is silently selected by the library default.

The configured mount must already exist and its identity must match. Native
macOS verification uses the volume UUID, not just `/Volumes/Active`. Verified
directory handles pin filesystem identity. Unix creation, append, publication
and removal use validated leaves relative to owned directory descriptors. A
replaced or offline directory cannot redirect a writer into a local stand-in.

Initial allocation is journaled before its filesystem effects. It publishes the
identity file, empty canonical file, location metadata and alias before allowing
producer use. Recovery completes only the exact recorded allocation.

A relocation records its exact source/destination, directory identities and
part sizes/digests. It copies and verifies before publishing the new location
and alias. Only then does it delete verified original parts. Recovery rejects
contradictory locations, symlink parts and conflicting destination contents. A
partial destination can resume only when its existing bytes are an exact source
prefix. Duplicate recovery does not increment the location generation again.

`recover_output_storage` handles the existing allocation and relocation
journals. It does not scan for orphan-looking files to delete. Week-old archival,
activity clocks, snapshot pruning and human cleanup selection are separate
consumers of this owner, not a second mover.

Active's existing unencrypted, ownership-ignored configuration is an explicitly
accepted downstream boundary. Normal local permissions and safe-export policy
still apply, but Unix mode bits on that volume do not provide stronger isolation
than the mounted volume actually enforces. Storage code does not change volume
ownership or encryption settings.

## Source reads

`SourceReader` streams a bounded UTF-8 presentation window and stores small
versioned continuation points, not a duplicate full ordinary source file. The
common character selector counts generated line labels as well as source text.
It uses Unicode scalar counts and the 70–130% nearest-line-boundary policy, with
a shorter-prefix tie break. Tiny targets fail rather than produce zero-progress
cursor loops. A changed ordinary source rejects its old point. A retry point
identifies the beginning of a page that was withheld rather than delivered.

Managed text outputs now use a bounded, hash-verified committed-prefix reader.
Canonical chunk hashes and committed length publish in one metadata transaction
after durable file writes. Reads validate only the bounded chunks they need,
not a complete duplicate of the source. Managed points use logical invocation
identity and remain valid after append or verified relocation. Ordinary file
points retain their strict filesystem-version checks.

Retained presentation exposes an exact structural continuation point when it
clips the body. Repeated retrieval uses stable coordinates. Withholding clears
a post-body continuation, and a withheld read page keeps its original retry
position. Cancelled source reads do not advance a delivered position.

Archive reads validate the original recorded volume identity without creating
archive directories or moving data back. The native owned-fixture test exercises
continuation before and after real relocation and rejects offline reads without
creating a fake mount. Older sealed outputs without a chunk index receive a
one-time streaming index import only when their original manifest digest and
committed byte count verify. The import stages chunk metadata, holds no database
write transaction during source scanning, and atomically publishes the index.
Cancellation or publication failure leaves source bytes unchanged and no partial
index. Missing original digests, changed bytes and legacy live ownership fail
explicitly rather than fabricating intact historical output.

PDF reads decode an exact in-memory source snapshot and retain the complete
selected derived page text, not a duplicate of the original PDF binary. Page
boundaries come from the decoder. Source byte count, SHA-256 and derived format
identity remain in metadata. Explicit line ranges select lines from the derived
text. A clipped result continues by reading its retained derived-text path and
read point; passing that point back to the original PDF is rejected rather than
silently rerunning extraction. Decoder failures and unsupported PDF builds are
failed outcomes, not successful metadata substitutes.

Images remain atomic typed media. The existing 20-MiB vision bound is checked
before acquisition, oversized images keep an explicit original-source reference,
and captured image bytes are retained unchanged. Terminal rendering uses the
same captured bytes rather than rereading a possibly changed file. Redirected
stdout does not emit terminal image escapes. Hosted-provider vision budgets and
unsupported formats remain explicit provider-boundary verification work.

## Runtime control

Every executing runtime registers a private, versioned control endpoint. The
metadata stores an instance identity, endpoint and kernel-held lease. Control
credentials are not serializable as public runtime metadata and are excluded
from debug formatting. Unix connections additionally verify the peer user.
Binding is exclusive, including the named-pipe path, rather than joining an
existing endpoint after an ownership conflict.

Status and control RPCs transfer metadata only. Stop acknowledges a request,
not actual quiescence. Wait returns a persisted terminal state. Dropping a wait
releases that subscription without cancelling the work. Completion racing with
runtime exit is recovered from the durable receipt. An unavailable or old owner
fails explicitly, never by guessing and signalling a PID. Kernel lease liveness
establishes endpoint ownership, not proof that arbitrary processes have ended.

The existing `bg` tool accepts an explicit durable run ID for `status`, `wait`,
`cancel`, `output` and `tail`. This makes individual foreground runs inspectable
and stoppable without waiting for the task-monitor UI. Output reads use the
committed prefix and preserve selected long lines and CRLF. Independent controls
can be batched. Background delivery flags and process-specific grace controls
still use their background delivery-task ID. User-facing background promotion
continues through the existing parent handoff path; the low-level ownership
primitive alone is not a claim that a parent turn has been resumed.

The command-worker/reload integration and full caller reconciliation remain
WP-02 work. Runtime tests use an ordinary native test process as an execution
owner, not a model agent or delegated implementation worker.

## Native command ownership and completion delivery

Unix Bash calls, including direct Tool callers, use the same Registry execution
and capture owner. Noninteractive calls
run in a native Jcode command worker, not an agent, and keep their invocation
identity when ownership transfers. Foreground calls with the existing stdin
request channel use the captured in-process command driver. Explicit background
selection and foreground timeout promotion return a durable acceptance receipt,
not a second execution or a claim that the command has finished.

A command worker claims its prepared invocation once. A gated child cannot exec
user code until the worker registers its private process group and exact boot
and process-birth identity. Full stdout/stderr bytes are drained concurrently,
with the canonical readable rendering and observed ordering retained by Capture.
Stop keeps the group leader unreaped through TERM/KILL and actual quiescence.
Denied stop signals do not become successful cancellation. Completed filesystem
effects are not rolled back.

The old Unix temporary-output/reload implementation is removed. Direct callers
receive the same retained-result or acceptance references as ordinary Registry
calls. Exit code, termination signal and timeout are stored as typed run facts
and sealed into recovery receipts. Background status can report them without
loading full output or parsing an error string; the conventional timeout shell
code is 124 while the actual process code/signal remain distinct. Scratch-directory
settings are applied by the shared command owner.

A background worker can survive the original runtime's reload. Foreground parent
loss interrupts the command. Compatibility background state is derived from the
durable execution, not the lifetime or preview text of a delivery wrapper.
The ordinary background manager can be recreated and still inspect or control
registered execution IDs. Legacy task storage remains a separate compatibility
boundary and does not gain ownership guarantees by interpreting an old PID.
New Unix detached registrations retain boot/birth-bound process identity.
Cancellation requires that identity, verifies the owned group actually quiesced,
and propagates terminal-receipt persistence errors. Old files without verified
identity fail before signalling. This compatibility safeguard does not claim
native Windows process-handle support or infer safe control from a reused PID.

Delivery flags and notification/wake outcomes live in the execution index.
Registration is idempotent and precedes model-visible acceptance. Missing parents
leave delivery pending, with no replacement session creation. Acknowledged
channels are not repeated. An interrupted attempt with uncertain acknowledgement
is exposed as uncertain rather than automatically causing another model wake.
Kernel leases distinguish active delivery attempts from abandoned ones.

Server delivery reuses original-parent notification and safe-point/wake owners.
Local TUI delivery acquires receipts outside the event loop, retains pending work
while busy, and rechecks the recipient before changing session history. It does
not inject an old session's completion into a newly selected session.

These mechanisms still require the complete final WP-02 caller, recovery,
producer and activation matrix. Other-platform command
paths, provider/SDK-supplied output, command progress and remaining clipped
producers must be reconciled before package acceptance. No claim of complete
WP-02 rollout or activated behavior follows from these library tests.

## MCP result and transport boundaries

MCP proxies retain complete received result objects, structured content, image
payloads, binary resources, RPC error data and result-decoding failures. Media
and resource parts have explicit file references and integrity metadata. A
producer-declared error is a failed tool outcome, not an apparently successful
text string beginning with an error label. Retained and withheld receipts link
to the authoritative manifest so rich parts remain discoverable without rerunning
the tool.

An MCP request owns its pending-map entry. Consumer cancellation, failed send or
protocol timeout removes that entry and attempts the standard request-scoped
cancellation notification. It never kills the shared MCP server to cancel one
call. Late replies cannot satisfy another request. Transport closure releases
pending callers and rejects new requests. Unpublished failed connections retain
ownership of their spawned process through normal child-drop cleanup.

Initialization and protocol discovery retain their bounded request timeout.
Tool execution does not inherit that fixed thirty-second handshake deadline:
tool-owned timeout arguments remain producer arguments, and explicit execution
control can stop waiting on the owned request. This is not a guarantee that an
external service stops remote computation or acknowledges cancellation.

## HTTP acquisition and converted output

`webfetch` retains received body chunks in an owned raw-response part before
format conversion. The selected HTML/text/Markdown rendering has no independent
40,000-character cap. Only the common presentation layer selects its returned
prefix. Raw parts use the same storage placement, backpressure and relocation
owner as command streams, with part identity and integrity in the manifest.

The existing five-MiB acquisition bound is separate from presentation. A larger
declared body is rejected before acquisition. If a streamed response exceeds the
bound, every chunk already delivered to Jcode is retained and the operation
reports incomplete acquisition instead of truncated success. Transport failure
similarly preserves the received prefix. HTTP error bodies are retained, not
replaced solely by their status code. Metadata reports acquired bytes and
completeness; unreceived upstream bytes are never invented.

HTML format conversion still selects prose, strips non-prose markup and renders
links under its existing conversion rules. The original acquired bytes remain
available independently, including invalid UTF-8 and omitted markup.

## Provider-supplied results and history

Results already produced by an SDK use a capture-only Registry operation. This
operation never invokes the registered native tool. The received payload digest
participates in replay conflict validation, while omitted native-input metadata
keeps old serialized invocation inputs byte-compatible.

Both Agent loops retain completed SDK text results before history presentation.
The MPSC loop emits the presented result, not an unretained full body. History
conversion no longer imposes a second 512-Ki-character cap and preserves the
structural error flag.

The existing partial-provider checkpoint now retains correlated SDK results
before writing their paired history receipts. A provider failure after receiving
SDK output preserves those effects and results even when the failure is not a
context-size error. A retry-rollback arriving after received SDK results stops
that automatic replay and checkpoints the results rather than discarding them.
These changes reuse the existing checkpoint and context-error owners. They do
not introduce a second transcript, automatic compaction or a new context policy.

This does not yet establish complete raw provider-ingress capture, uncorrelated
result recovery or native-SDK fallback semantics. Those boundaries remain in the
producer reconciliation ledger and final WP-02 verification scope.

## Native progress and metadata ownership

Command drains retain raw bytes before interpreting progress. The existing Bash
marker and heuristic parsers feed an execution-owned monotonic progress record.
Status parsing is bounded independently of capture, so oversized or invalid
marker lines remain in the raw output even when they are not interpreted as
progress. Status messages/units are short projections; they are not replacements
for the captured output.

Durable `bg wait` supports progress/checkpoint return without owning or stopping
the command. Current progress survives manager recreation. The original runtime
receives native-worker progress events, and in-process captured commands publish
through the same ordinary background event channel. Shared equivalence and
informativeness rules avoid timestamp-only updates and less-informative parsed
status replacing reported progress.

SQLite exclusively owns database descriptors. Do not pre-open/close index.sqlite
through ordinary filesystem APIs in a process with live SQLite connections:
POSIX close semantics can release another connection's locks. The execution
store uses SQLite's NOFOLLOW open after resolving ancestor aliases, and hardens
permissions before WAL/SHM creation. Real cross-process tests cover lock
preservation in both WAL and rollback modes plus symlink/private-mode checks.

## Provider request cancellation

Attempt forwarders own their tasks through normal finish and cancellation.
Closing an idle consumer closes the attempt channel immediately, without waiting
for another provider event. Dropping or cancelling the finish future does not
detach the forwarder.

Anthropic Messages (including split requests), OpenRouter-compatible HTTP,
Copilot and Bedrock request tasks now observe receiver closure through a shared
request-lifetime owner. A quiet network read or retry backoff is cancelled by
dropping only that request future. A real local HTTP provider test verifies the
connection closes and the same provider remains usable for another request.

Gemini and Antigravity use the same receiver-bound request lifetime. Their cached
state is authentication/setup state, not an in-flight response chain. Cursor's
HTTP/2 connection driver and paced sender are request-owned subtasks, so an
error or consumer drop cannot leave them detached. A deterministic test uses the
actual Cursor HTTP/2 operation over a local duplex transport and verifies closure
without another request. Existing frame/parser tests and strict lint pass.

OpenAI quiet SSE and WebSocket reads observe receiver closure directly. For a
persistent WebSocket, cancellation reaches the existing response-chain cleanup
while its state lock is still held, rather than clearing a possibly unrelated
later request from an out-of-band callback. An in-flight response-chain lease
also clears that exact state if its producer future is dropped during a send or
read. Completed chains remain reusable. The whole OpenAI request producer is
receiver-bound, including connection establishment and retry backoff. Quiet and
abrupt-cancellation continuation tests pass. Remaining CLI/web subprocess and
pre-dispatch caller boundaries still require final reconciliation. These checks
do not prove that a hosted service acknowledges cancellation or stops remote
compute, which remains outside the approved owned-work boundary.

## Scoped historical tool-result recovery

Missing-result repair pairs tool uses and results in transcript order, scoped by
session and assistant-message identity rather than a process-global raw tool ID.
A completed retained result or background acceptance is retrieved from its exact
invocation and checked against the historical input before it is inserted. This
never calls the original producer. An earlier equal provider ID cannot satisfy
a later message, and an unrelated session's running call cannot suppress repair.

The Registry dispatch and its surviving execution supervisor retain scoped
in-flight guards. Active or unresolved owned work blocks a new provider request
rather than receiving a fabricated result. The repair does not advance past
unresolved work. Existing Session persistence and context reconciliation still
own history mutation, rollback and deliberate cache invalidation. Legacy calls
without retained records keep their historical missing-output disposition.

## Lost-owner recovery

Runtime registration records a process-image token separately from PID and
endpoint identity. An absent/unleased endpoint alone never proves that work
stopped. Recovery requires a terminated process or a verified replacement image,
an exclusive lease for the exact output, and no live members of a recorded
native command group. Unverified legacy ownership, live capture, live groups and
offline storage remain explicit failures, not successful cancellation.

An existing sealed witness is validated and restores its actual terminal outcome.
Otherwise recovery reuses recorded per-run allocation/relocation operations and
publishes `Interrupted`, preserving original input and the committed output prefix.
It writes a separate immutable interruption receipt instead of overwriting the
original output or claiming unknown filesystem effects were rolled back. This
receipt uses common character presentation and identifies any partial output.

Registry replay and authoritative history repair use this operation without
starting the original producer. Replay waiters cannot act as the old producer or
wait on themselves. Background delivery reconciliation recovers proven lost
owners and sends the failure receipt only through the original-parent delivery
machinery. Metadata-only status remains independent of full result retrieval.
Native acceptance includes real process exit without Rust destructors, refusal
to retire live groups/captures, original input/prefix preservation, terminal
witness precedence, Registry/history replay and original-session delivery.
Unsupported owner-loss platforms remain unproven before storage mutation; no
native Windows parity is claimed by the macOS tests.

## Verification

Run through coordinated self-development tests:

```sh
scripts/dev_cargo.sh test --profile selfdev -p jcode-base --lib execution:: -- --test-threads=1
scripts/dev_cargo.sh clippy --profile selfdev -p jcode-base --lib --tests -- -D warnings
```

The tests use real SQLite/filesystem owners with controlled capacity, write and
publication faults. They cover every exposed allocation/move interruption
boundary, exact partial-write preservation, duplicate recovery, directory and
symlink replacement, unavailable storage, terminal retry/corruption and
independent-process invocation admission. These subprocesses are ordinary test
executables, not model agents.

The ignored native archive fixture requires an explicit
`JCODE_EXECUTION_ARCHIVE_FIXTURE` JSON file describing a new isolated directory
whose name starts with `jcode-execution-fixture-`. Invoke its exact test with
`--ignored`. It verifies the real volume, initial emergency placement, writer
relocation and stable alias reads, then removes only its own fixture. It forces
the reserve policy, not actual host disk exhaustion. Never point it at user
output data or treat a skipped native check as passing evidence.

Current mandatory native acceptance is macOS arm64. Other native platform
coverage and capability limitations must be recorded explicitly during rollout.
