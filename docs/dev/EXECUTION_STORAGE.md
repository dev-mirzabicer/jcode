# Execution storage internals

This describes the shared storage/read owner in `jcode-base::execution`, accepted
with Phase 4 WP-02 on 2026-09-13. Registry, batch members, provider/SDK acquisition,
native command/helper ownership and local/shared callers use the owners described
below. Adapter evidence and platform bounds are explicit in EXECUTION_PRODUCERS.md
and EXECUTION_ACCEPTANCE.md; library tests alone are not a native workflow claim.

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

Schema 21 adds execution-parent and session/state/time indexes without changing
existing run, input, output, activity or child records. Task pages seek by exact
state and applicable session/parent partitions, merge only bounded candidate IDs,
and keep the existing newest-first `(created, id)` cursor. Nested-row discovery
uses the parent index instead of scanning the complete run history per row.
An older binary that does not understand the new schema rejects it. Do not
downgrade the live database or run candidate tests against the live home.

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

The shared [physical-location boundary](PHYSICAL_LOCATIONS.md) now owns native
volume UUID/mount verification and available-byte inspection. Archive configuration,
serialized directory witnesses, descriptor-relative I/O and recovery remain here.

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
activity clocks, snapshot pruning and human cleanup selection now use the
[inspection/retention owner](SESSION_INSPECTION.md), not a second mover.

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

## Output configuration

The existing configuration owner accepts `[output]` with optional `default_size`,
`[output.per_tool]` entries, and `[output.storage]` placement settings. Presentation
uses per-call `output_size`, then a per-tool entry, then `default_size`, then the
shipped read/general targets (40,000/20,000 Unicode scalar characters). Size aliases
are `very_small`, `small`, `medium`, `large`, and `very_large`; a positive integer
is also valid. These are presentation targets, not acquisition or retention caps.

Per-tool configuration keys normalize through the same canonical tool aliases as
execution. Multiple keys resolving to one tool are rejected rather than resolved
by map order, even if one uses a provider namespace or alias. Empty or whitespace-
padded keys fail decoding. Dynamic tool names remain possible without requiring
that an external tool be connected when configuration is loaded.

Storage reserves remain configurable byte values. Archive selection is explicit
and bound to a mount plus volume identity and dedicated relative directory. Source
defaults do not imply that Mirza's live serialized settings have been changed;
configuration cutover and activated-runtime evidence are separate acceptance steps.

## Remote materialization of retained parts

Harness v1.3 adds `shared_execution_parts_v1`. Rust and TypeScript SDKs require
that capability, in addition to `shared_execution_v1`, before sending an execution
`read_part` request. Older servers therefore reject locally without receiving a
request they cannot implement. Existing execution request/reply session correlation
and same-user attachment authorization remain unchanged.

Select a sealed run and a part name, never an arbitrary server filesystem path.
Read `manifest.json` to discover image/resource decoded filenames and declared
raw parts. New captures include integrity for raw stream/event/control parts.
Other names must be declared in the manifest with original integrity evidence.
Decoded resources/images are checked against their original encoded part before
returning bytes. Legacy parts without that evidence fail explicitly.

Responses carry base64 bytes, offset, total length, SHA-256 and optional next
offset. The default is 64 KiB per page, with an explicit 1-byte to 1-MiB range.
Continuation requires the preceding SHA-256. Reads verify the selected part before
returning a bounded page and reject corruption or changed continuation versions.
The reader hashes through bounded buffers, retaining only the requested page;
it currently verifies the whole selected part on each page, rather than claiming
constant-time random access. Ordinary execution lists/status/control remain
metadata-only and canonical live output keeps its indexed text reader.

Ordinary `read` also recognizes declared sealed bundle parts, including the
manifest and base64/text parts, as logical managed sources. Their read points bind
to both the part path and content digest, so relocation preserves continuation
while changed metadata/bytes reject an old point. Canonical live output retains
its separate append-safe indexed behavior. Part verification and decoding observe
Stop without creating a zero-progress retry loop. Manifest projections ignore
unrequested large metadata and use buffered input; image retrieval shares that
projection. Withheld read metadata sets `advanced=false` consistently with the
unadvanced retry position. Existing ordinary-source version checks remain intact.

Verified storage opening preserves alias/archive identity and reads archived parts
in place. Tests reconstruct non-UTF-8 stdout and resource bytes, reject traversal,
undeclared parts, missing/stale hashes and corruption, and continue a binary image
page after a real owned archive relocation. The full Harness/Rust SDK suites,
49 TypeScript tests, and strict affected lint pass. No producer is rerun and the
SDK does not write a client file without caller-directed materialization.

## Reading retained image parts

Canonical `execution/outputs/<run-id>/image-<index>.bin` references are recognized
by `read` as atomic media, not generic binary placeholders. The storage owner
requires a sealed matching invocation/manifest, verifies the logical alias and
recorded physical/archive identity, and opens parts through the bound directory.
It verifies the original encoded image's byte count/digest and compares decoded
bytes with the binary part before delivering pixels. It does not infer a media
type from a `.bin` filename or accept changed backing bytes as the original.

The existing 20-MiB atomic-image bound applies before binary acquisition. Manifest
image/resource descriptors expose `decoded_file` only when the received base64
was actually decodable. Older descriptors remain readable when their original
integrity evidence is sufficient. Invalid/unsealed/offline references fail rather
than rerunning the original tool. Rendering uses the same captured-byte image
renderer as ordinary image reads.

A public Registry/read regression reproduced the original zero-image placeholder
and now checks exact pixels, media type, encoded and decoded corruption, and the
atomic size bound. The isolated native Active fixture verifies identical image
retrieval after relocation and rejection of an offline recorded archive. Store
regressions and strict base/app-core lint pass. No original producer is repeated.
SDK binary-part materialization and retained-manifest paging use the separate
capability-gated byte-page path documented above, not the text-only output reader.

## Background summary and reload views

The local TUI collects legacy in-process and durable managed background metadata
in an off-render worker with at most one refresh in flight. Rendering reads a
cached projection, not SQLite or output/status files. Replies are bound to the
state-root namespace; stale replies cannot replace another namespace's view.
Refresh failures retain explicitly last-known metadata and produce a human status
notice. Metadata collection does not fetch output bodies or count as agent work.

Reload recovery notes include managed execution IDs after manager recreation,
including jobs with notifications disabled. The note describes unfinished
records rather than pretending that a saved PID proves a live process. Actual
status/control and owner-loss recovery retain their existing proof boundaries.
Unreadable execution metadata produces explicit retrieval guidance rather than
an invitation to repeat previously accepted work.

Tests cover recreated managers with unavailable output bodies, exact session
filtering, terminal removal, local asynchronous cache updates, stale-root reply
rejection, failure visibility, and existing reload-directive behavior. Strict
base/app-core/TUI lint passes. Activated native TUI frame verification is recorded
separately in EXECUTION_NATIVE_ACCEPTANCE.md rather than inferred from these tests.

## Generic native helper process provenance

Nongated native commands reserve a process launch receipt through their existing
capture owner before spawning. Registration records the actual PID and available
boot/birth identity before output draining starts. If a very short-lived process
cannot supply that identity, the failed observation is recorded rather than
inventing one. These receipts are metadata, not a second supervisor. They never
authorize recovery to signal a bare PID.

Failed spawn or Stop before spawn closes its launch intent. Otherwise the command
owner records completion only after the entire private group has no live members
and its leader is reaped. Normal completion now checks quiet descendants too:
pipe EOF and leader exit alone do not release ownership. An impossible deadline
fails before reserving or launching work. Capture sealing refuses unresolved
native process receipts, so aborting a caller cannot manufacture a terminal
witness while owned work remains unproven.

Owner-loss recovery inspects recorded helper groups as well as gated command
workers. A live helper prevents retirement. Once its group is actually quiescent,
recovery can preserve the committed prefix and publish interruption without
replaying effects. A crash between launch intent and PID registration stays
explicitly unproven. Legacy nonterminal runs without complete process-tracking
metadata also stay unproven rather than acquiring invented process history.
Existing sealed outcomes retain their normal terminal-witness recovery behavior.

A real process-exit regression reproduced the former false retirement while its
Python helper was still running. Tests now require refusal while live, recovery
only after fixture cleanup proves quiescence, preserved prefix bytes, exact
receipt ownership, pending-launch and legacy refusal, quiet descendant completion,
failed spawn and deadline rejection. Computer/search helper families, execution
store tests, integrated execution tests and strict affected lint pass on macOS.
This does not claim adversarial process containment or native Windows parity.

## Self-development build and test jobs

Build-producing, test and attached-watcher requests use the common execution owner
with durable run IDs, capture, completion delivery and explicit background
acceptance. The existing worktree queue, source fingerprint validation, build lock,
deduplication and publication checks remain with the self-development coordinator.
The job records its complete command arguments and original request identity.
Delivery and request linkage are durable before the caller receives readiness.

Queue messages and native output share one append-only capture; command startup
cannot truncate an earlier queue receipt. Unix subprocesses use the existing
process-group supervisor, concurrently retain raw stdout/stderr and keep the build
lock until actual quiescence. Output write/acquisition errors propagate rather than
being treated as EOF or success. Stop is observed while queued and before later
publication actions. Completed effects are not rolled back. Watchers only observe
the original build: cancelling one watcher does not stop that build.

A capture-allocation failure leaves a failed request rather than an orphan queued
entry. Cancellation reloads request state after awaiting control, so a natural
completion is not overwritten by a stale cancellation snapshot. Superseded builds
retain a separate durable `superseded` facet while execution state remains
`completed`; manifests/recovery and metadata-only background status preserve it.
Compatibility status files are refreshed from the authoritative record, including
terminal work with notifications disabled.

Tests cover preserved queue text and invalid UTF-8 bytes, actual TERM-resistant
child stopping, released compiler leases, watcher isolation, startup allocation
failure, superseded state and terminal-receipt recovery. Forty selfdev tests and
59 execution-store tests pass (one explicit native archive fixture is not run in
that matrix), as do strict affected lint, both host checks and 48 TypeScript tests.
These tests run synthetic commands, not a real source publication. Generic helper
process-loss proof and local summary/reload views are covered above. Native
activation and canary passed as recorded in EXECUTION_NATIVE_ACCEPTANCE.md. Unsupported non-Unix build-command
ownership fails explicitly before launching a process rather than using the old
unmanaged command path; native platform parity is not claimed.

## Native helper commands

Computer-use and Agentgrep helper commands share the native command supervisor
through an explicit invocation-owned `HelperHost`. The context carries capture,
cancellation and working-directory state without thread-local or global policy.
Computer Tool callers enter Registry ownership even on direct invocation. Blocking
computer work remains joined while Stop reaches its actual helper process group.

Stdout and stderr are drained concurrently before waiting for process completion.
Raw helper streams remain named output-bundle parts, independent of formatted
AppleScript/AX/search results. Clipboard input and Swift OCR source are retained
as owned stdin parts and passed by file descriptor, avoiding a pipe writer that
can deadlock before the output drains start. Previously timed calls preserve their
deadlines; untimed helper calls do not acquire an implicit deadline. Permission
and wait polling observe Stop between operations. Atomic CoreGraphics input
semantics are unchanged.

A red regression reproduced a 200 KB dual-pipe timeout in the former OSA helper.
The repaired owner passes that test, a 300 KB stdin round trip, large script-output
retention, actual script Stop with preserved earlier filesystem effects, and
existing search-helper cancellation. Thirty computer tests and 34 Agentgrep tests
pass with strict app-core lint. Eighteen live GUI/permission tests remain explicitly
ignored; no desktop interaction was performed for this verification.

Firefox browser action helpers also use this owner on Unix, including direct Tool
callers. Each helper receives a distinct output-part namespace, so sequential
bridge responses cannot be concatenated accidentally. Raw streams are retained
before text/JSON conversion; invalid UTF-8 is an explicit decode failure. Shared
browser-session registration finishes its existing bounded startup before action
cancellation is observed, rather than killing a persistent shared bridge by guess.
Status/setup retain their separate existing readiness and installation owners.
A local fake bridge verifies full large responses, malformed raw bytes and actual
helper cancellation with no later filesystem effect. No real browser interaction
is performed by that test.

## Rejected tool admission

Registry availability, session-policy, unknown-tool, input-binding and execution-
policy failures are retained as scoped foreground failure receipts. They use the
same input/output store and context delivery guard as other outcomes, but do not
run the rejected producer, its hooks, a source read or a background launch. Replays
retain the original invocation identity rather than executing a newly permitted
operation under an old receipt. Missing invocation identity or unusable storage
still fails explicitly rather than claiming a retained receipt exists.

Tests verify exact rejected input, retained failure output, zero producer effects,
and continued global Swarm retirement, including aliases and batch members.

## Client and Harness execution API

Harness protocol v1.2 advertises `shared_execution_v1`. Rust SDK
`client.execution(session_id, request)` and TypeScript
`client.execution(sessionId, request)` require that capability before sending.
The internal daemon request is `execution { id, request }`; its correlated reply
is `execution_response { id, response }`. The bridge retains the original request
session even if the connection attaches elsewhere before the reply arrives.

Supported request actions:

- `list`: current-session metadata by default, or explicit `all_sessions: true`.
  `limit` defaults to 50 and accepts 1–200. `next` is passed as `after` to fetch the
  next stable ID-ordered page. Listing never loads input/output bodies.
- `inspect { run_id }`: the canonical metadata snapshot, including current state,
  progress, stop cause, ownership and available output facts.
- `stop { run_id }` and `background { run_id }`: route to the verified live owner.
  `accepted` is an acknowledgement, not proof of terminal quiescence. Inspect the
  state afterward. A control never starts or repeats the producer.
- `read { run_id, content: input|output, read_point?, output_size? }`: validates the
  owned input or output source and pages text using the common reader. Input
  inspection validates the complete original invocation before paging. Output
  reads use committed/hash-verified managed prefixes. Use the page's exact next
  point to continue. Missing-source and stale-point failures remain explicit.

Paths returned in metadata describe server-owned storage. Remote clients retrieve
text through the read operation rather than assuming those paths exist locally.
The native task monitor uses the same execution owner, with its own bounded
metadata/text projection. Raw runtime control credentials are not exposed. The
Harness API requires a completed authenticated session attachment. Unknown fields
reject, and the additive `force_stop` operation requires `execution_force_stop_v1`.

Tests exercise metadata paging with unavailable bodies, input/output retrieval,
SDK rejection before transport on old capability sets, duplicate/cross-session
reply correlation, and the actual daemon inspecting, reading and stopping a live
run while its attached Agent mutex is held. TypeScript schema/client tests and
strict affected Rust library/test lint pass. This is not a claim of native
Windows process-control parity or final WP-02 activation.

## Runtime control

A closed compatibility-delivery channel is not proof that an execution lacks a
result. The background waiter resolves the actual authenticated execution owner
and durable receipt after channel closure. A still-running owner remains running;
closing or timing out this wait does not cancel it. A completed receipt remains
recoverable even if its earlier notification channel disappeared.

Managed control registrations are keyed by execution-store path and invocation
ID. Delayed completion publication retains the store captured at registration,
rather than resolving whichever home configuration happens to be current later.
Tests use the same logical run ID in two private stores with opposite outcomes to
prove one cannot replace the other's result or remove its live control. Reload
still examines all registered owned controls and refuses unproven quiescence;
this is not a shortcut that discards missing or uncertain work.

Every executing runtime registers a private, versioned control endpoint. The
metadata stores an instance identity, endpoint and kernel-held lease. Control
credentials are not serializable as public runtime metadata and are excluded
from debug formatting. Unix connections additionally verify the peer user.
Binding is exclusive, including the named-pipe path, rather than joining an
existing endpoint after an ownership conflict.
On Unix, an oversized temporary path uses `/tmp/jx-<uid>` for only the short IPC
endpoint. The directory must still be owned by the user with no group/other
permissions. Durable runtime leases, credentials and records remain in the
original execution namespace. This avoids macOS Unix-socket path overflow.

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

Command-worker/reload and caller integration use the owners described in this
document. Runtime tests use an ordinary native test process as an execution
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

The accepted WP-02 caller/producer and native evidence is recorded in
EXECUTION_ACCEPTANCE.md and EXECUTION_NATIVE_ACCEPTANCE.md. Later delegation,
inspection and task-monitor integrations retain these owners. Historical evidence
is revision-bound and does not replace combined verification of current source.

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

Acquisition-time SDK receipts, adapter capture context and explicit host-native
selection below supply the earlier-ingress, unmatched-result recovery and native
fallback boundaries. This history conversion does not independently own them.

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
Opening an already-current schema does not request a SQLite writer transaction.
First-time WAL/schema initialization is serialized by a separate private kernel
lease, since concurrent WAL-mode transitions can fail before ordinary busy
waiting applies. Connections leave an already-selected WAL mode unchanged.
Regression tests cover fresh concurrent opens, metadata opens while a writer is
active, and real SDK acquisition observed during an unfinished provider stream.

## Acquisition-time SDK receipts

Agent and local TUI consumers durably capture each received SDK result before
polling the provider again, rather than waiting for the response stream to finish.
These are explicitly named acquisition records, not fabricated executions of the
SDK tool. They retain the full result, media, original decoded record when supplied,
and observed matching tool inputs through the existing output/storage owner.
Later tool-result publication has its own assistant-message-scoped identity and
records the acquisition reference before correlation. Host-native rejection
handling retains the same reference before any authorized host effect.

A private kernel lease owns each request's unacknowledged acquisitions, including
blocking capture that outlives a cancelled consumer. Active requests cannot be
recovered as abandoned. Recovery after actual process exit retains available data
and emits factual references for unmatched results without inventing missing tool
input, completion or rollback. Duplicate and uncorrelated results are captured
before automatic retry is stopped. Storage failures preserve complete received
bodies through existing authoritative partial checkpoints; an unretained storage
error is never substituted for the original SDK result.

Receipt acknowledgement uses compact Session/journal metadata bound to the
execution-store namespace. It is published atomically with history, survives
ordinary resume/rewind, remains present in lightweight and remote startup metadata,
and is not inherited as a parent's receipt ownership during split. Namespace
replacement and regressed metadata are checked explicitly. Already paired results
are acknowledged only against their actual assistant ToolUse and later user
ToolResult, not a process-global provider ID.

Tests cover paused provider streams in both Agent modes, acquired rich results,
errors and rollback, duplicate/unmatched IDs, unavailable storage, request leases,
process exit without destructors, acknowledgement/reload/rewind/split, and local
TUI capture. Bytes acquired before CLI decoding use the adapter capture owner
below, not this consumer-stage receipt service. Data never acquired from an
external producer is not invented.

## Provider adapter capture context

Provider request context carries a host-owned result-capture callback separately
from prompts, tool definitions, authentication and provider configuration. The
Agent, local TUI, provider wrapper, route dispatch and account-failover paths
forward this context explicitly. No task-local or mutable provider singleton is
used. A received-data flag prevents automatic request/account/provider retries
after an adapter has observed SDK data, even if capture or stream establishment
then fails.

With this context, Claude CLI retains a decoded SDK result before publishing it
to the event channel. Its host-issued receipt is checked against the exact
request, store namespace, recipient and result digest when consumed. Observed
complete tool inputs are recorded independently and contradictory consumer input
is rejected. Receipt references are not parsed from model or remote JSON fields.
An unscoped CLI request with SDK tools rejects before spawning a process instead
of silently claiming the new capture guarantee.

CLI stdout records are read as bytes before UTF-8/JSON decoding. Malformed JSON,
invalid UTF-8, and declared SDK-result blocks that cannot be decoded completely
stop the owned process and retain the original acquired bytes as a typed binary
resource, including remaining buffered stdout. Consumer cancellation before a
record's newline preserves the acquired partial bytes. A failed capture of a
valid SDK result still supplies the complete original event for the existing
Session fallback, then stops retries. Ordinary valid-record normalization and
prompt composition remain unchanged.

Tests exercise real local CLI processes, malformed and incomplete records,
buffered tails, partial-record cancellation, original input provenance, callback
ordering before queue consumption, exact receipt reuse and changed-input/ref
rejection, and both Agent loops before/after stream-publication failures. Both
host executables compile and strict affected-library/test lint passes. These
checks do not claim hostile subprocess isolation, vendor acknowledgement, or
native non-Unix process-tree cancellation.

## SDK original records and rich results

Decoded CLI tool-result events carry the original UTF-8 protocol record alongside
the selected content. The record preserves surrounding record whitespace and
unknown envelope/result fields; the line-delimiter framing is not part of it.
One shared conversion retains that original in the private result manifest and
extracts matching typed image/resource parts without interpreting unrelated tool
results as attachments. Unknown content stays available in the original record.

Both Agent loops carry the complete result through normal and partial-provider
checkpoints, including errors and retry rollback. A shared history converter
preserves attached images and error state. Storage-failure fallback includes the
original record rather than only its normalized text. Native SDK rejections also
keep the received record before any code-authorized host execution.

Local TUI SDK handling now uses the same retention boundary before debug/display
publication and persists the actual result instead of an empty Session placeholder.
Local partial checkpoints retain complete results and media through this owner.
These are real CLI, Agent and local-TUI mechanism checks, not a hosted vision or
prompt-quality claim. The request-local adapter capture above additionally retains
malformed CLI records before failure. Acquisition-time receipt recovery remains
the owner for correlating or reporting these received records.

## Local history repair and /fix

Agent turns and local TUI turns now use one scoped history scan and repair
transaction. Preparation resolves retained receipts asynchronously without writing
Session history. Publication checks the complete source Session fingerprint,
uses existing context reconciliation, persists the candidate, and only then
replaces live authority. A stale result cannot overwrite newer history, context,
metadata or a different session. Active or ambiguous tool work remains blocked
rather than receiving an invented completion.

Local `/fix` prepares on the runtime and applies at an idle local tick. User drafts
remain untouched. Busy work, a switched session, a newer error and a stale source
cannot be overwritten. `/fix` no longer invokes the text-only replacement-session
fallback when tool history is unresolved. Explicit context reduction still belongs
to the existing Context Editor. A remote client never saves its partial Session to
perform repair; server-side Agent repair remains authoritative, and this local
command refuses that unsupported client-side action clearly.

Tests exercise actual submit/tick handling, complete retained results, live work,
newer metadata, switched sessions, remote shadows, resume reset and protected
context-budget/reconciliation behavior. The old text-only recovery helper remains
only in historical mechanism tests, not an active `/fix` branch.

## SDK host-native execution

Host-native selection comes from the active provider's explicit tool exclusions,
not a global two-name list or recognition of error prose. Claude CLI uses the
same exclusions for its actual `--tools` selection and its host-native declaration.
Provider wrappers forward that declaration without changing model policy.

An SDK error permits host execution only when that SDK structurally excludes the
tool. The complete rejection is retained as an auxiliary output part before host
effects, and its digest participates in invocation replay identity. A conflicting
rejection cannot cause a second execution. An opaque SDK error from any other
route, or an already-reported successful result, is retained without executing
the local producer. Both Agent loops and the CLI exclusion mechanism are tested.
Raw CLI ingress and unmatched-result preservation use the separate acquisition
and adapter owners above; native fallback cannot substitute for those receipts.

## Optional ChatGPT web route

The web route now observes consumer closure while waiting for per-provider
admission, browser readiness and the active response operation. Existing owned-tab
cleanup still runs after active-operation cancellation. An in-flight tab allocation
is allowed to finish so its actual identity can be cleaned up, rather than dropping
a remote creation call and guessing which tab exists. Existing bridge process
kill-on-drop and bounded cleanup behavior remain unchanged.

This is a focused correction, not a browser transport redesign. Deterministic tests
cover cancelled admission and active-wait drop/cleanup reachability; existing web
parser tests and strict provider lint also pass. No live ChatGPT account, browser
page lifecycle, hosted computation termination or broader web reliability was
validated by these checks.

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

Claude CLI cancellation also covers serialized admission, retry backoff, input
writes and a quiet process wait. Unix CLI processes own a private process group;
Stop targets that group and waits for quiescence before reaping the leader.
Request-local stderr tasks are not detached, and raw stderr is no longer copied
into ordinary debug logs. Native tests use a local fake CLI with TERM-resistant
descendants and verify that cancellation preserves earlier filesystem effects
without allowing later effects. Full raw CLI ingress retention and non-Unix
process-tree guarantees remain separate from these lifetime checks.

OpenAI quiet SSE and WebSocket reads observe receiver closure directly. For a
persistent WebSocket, cancellation reaches the existing response-chain cleanup
while its state lock is still held, rather than clearing a possibly unrelated
later request from an out-of-band callback. An in-flight response-chain lease
also clears that exact state if its producer future is dropped during a send or
read. Completed chains remain reusable. The whole OpenAI request producer is
receiver-bound, including connection establishment and retry backoff. Quiet and
abrupt-cancellation continuation tests pass. CLI/web subprocess and
pre-dispatch boundaries have separate evidence in the producer ledger. These checks
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
