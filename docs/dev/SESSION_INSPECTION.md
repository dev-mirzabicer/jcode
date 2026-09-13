# Session inspection and retention internals

The execution store owns immutable inspection artifacts and the operational
activity clock. Session remains the only mutable conversation. Context projection
continues to use `jcode-context-core::project_context`; inspection does not apply,
undo, migrate or rewrite a context transaction.

## Coherent capture

Session checkpoint, journal append, ordinary load and read-only capture share a
short kernel persistence lease. It is independent of the running Agent mutex.
Capture reads the authoritative snapshot plus complete journal entries. Corruption,
torn entries, inconsistent append coverage, and unsupported active legacy writers
fail explicitly rather than being silently salvaged. Ordinary restoration retains
its existing separate migration/recovery behavior.

Checkpoint epochs pair snapshots with their journals. A checkpoint records the
digest of the old journal it incorporates, durably publishes the replacement, and
only then removes that journal. If a process exits between publication and unlink,
the exact retired journal is not replayed twice. An unrecognized journal is never
discarded merely because it appears older. New journal writes are flushed before
publication. Stale checkpoint writers fail instead of overwriting another epoch.

Snapshot creation captures all applicable execution metadata in one SQLite read
transaction while the Session storage lease is still held. It releases both before
projection, content-addressed publication, or full output acquisition. Thus tool
status and prefix references belong to the same committed conversation boundary,
not an Agent that happens to be available later.

## Immutable artifacts

The local execution database indexes reader, target, creation order, retained or
pruned state, blob references, and referenced executions. Message, projected-message,
context-view and instruction blobs live under `execution/snapshots/blobs`, addressed
by SHA-256. Unchanged content is shared between snapshots. Blob ownership is
registered before publication, and reclamation uses those registered references,
not an orphan-looking directory scan.

An outline returns a `snapshot-...` identity. Transcript and tool expansion require
that identity. A new outline captures a new snapshot. Source edits, rewind, reload,
source removal or later output growth never retarget an old snapshot.

Projected ranges use one-based original message positions. If a summary intersects
the requested range, its complete text and actual source coverage are returned.
Explicit raw mode returns stored source messages. Media, reasoning, active
instruction identities and legacy availability are represented structurally.
Expansion uses snapshot-bound tool references, not ambiguous raw provider IDs.
Nested executions retain their own full invocation identity. A captured running
output remains bounded to its captured prefix and status after the producer ends.

## Activity and pruning

The operational clock is in `session_activity`, not Session.updated_at or filesystem
access time. New messages, actual execution and explicit session opening/resumption
count. Live model turns hold kernel-owned activity leases, and nonterminal tool
runs also exclude their sessions from idle maintenance. Listing, status polling,
metadata saves and maintenance scans do not refresh the clock.

Agent A inspecting B updates A, not B. Explicit human Session inspection also counts
as use of B. This does not grant control of B or make inspection a profile activation.
The trusted-client control boundary does not attest that a physical human, rather
than an authorized script, sent a request. No additional human-attestation or OS
sandbox is provided.

After a reader's idle week, pruning keeps its newest two snapshots for each distinct
target. In-flight snapshot readers hold ownership through their backing reads.
Pruning removes only snapshot references and unreferenced registered blobs. Pruned
IDs retain a truthful disposition. Already delivered inspection results and either
session's history are unchanged.

A lost model-turn owner has no trustworthy exact finish timestamp. Maintenance
records the first proof its kernel lease is unowned without changing last_active.
That uncertainty exclusion expires after a full week from that fixed observation.
Repeated scans never refresh the observation. Normal completed turns record their
actual end activity and release the lease immediately.

## Cold archival and cleanup

Sealed outputs become eligible after the owning session's idle week. Cold archival
uses the same copy, hash verification, location publication, alias replacement and
source-removal journal as emergency spillover. Archived output stays on its verified
volume when a session resumes. Routine offline archival is deferred, not retried as
a producer execution, and no local substitute mount directory is created.

Active is bound to its configured UUID and dedicated archive namespace. Its existing
unencrypted, ownership-ignored protection level is an accepted downstream boundary.
The storage owner does not change volume encryption or ownership settings.

Cleanup reviews select exact whole completed cold-archived outputs. Live work and
recent emergency spillover are excluded. Oldest-byte selection reports its exact
total and any overshoot. The review also identifies affected snapshot IDs and states
that prior delivered history, invocation inputs, Session data, project files and
child-authored artifact directories are not removed.

One confirmation is bound to the exact review and its originating trusted client
session. Changed identity, contents or snapshot impact makes an unconfirmed
review stale. Confirmed deletion is journaled and retryable, with per-output failure
outcomes. A failed item can already have partial physical deletions; `deleted=false`
means durable completion has not been established, not that no effect occurred.
Recovery resumes only confirmed plans. Merely reviewed plans never execute through
maintenance or reload. Output tombstones distinguish deliberate deletion from an
empty result or an offline volume.

The runtime's quiet maintenance worker uses the originating execution-store namespace
and captured storage configuration. A kernel lease serializes independent runtimes.
It does not wake a model or send routine successful relocation notices. Human status
is metadata-only and does not refresh session activity.

## Verification boundary

Mechanism tests use synthetic conversation content, real Session/SQLite/filesystem
owners, kernel locks and injected storage failures. They do not judge prompt or skill
wording. Protocol, SDK, real-volume and activated-runtime evidence must be recorded
separately from these library tests. Native mandatory acceptance is macOS arm64;
other-platform guarantees must not be inferred from it.

## Migration and client integration

Cold classification is retained after a session resumes. Opening/inspecting a
session does not hide its already sealed cold-archived outputs or independently
invalidate a review. Live output ownership, actual content/location changes and
new snapshot references remain checked.

Schema 19 adds a one-time retention observation floor for migrated activity rows.
It leaves their recorded last activity unchanged and prevents archival/pruning
until a full week of the new tracker is available. New activity then controls
ordinary eligibility. Reopening a current store does not refresh this floor.

Production inspection rendering streams through the common execution capture and
Stop owner. The trusted-client adapter uses that same supervisor. Connection-owned
request tasks keep metadata/Stop responsive while another inspection waits for
storage. A small captured part preserves snapshot identity if later rendering or
Stop interrupts the body. Primary descendants can inspect ancestor-owned snapshots
without changing ownership or refreshing the ancestor's activity clock.
