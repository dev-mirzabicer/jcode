# Recoverable workspace catalog

The catalog is the private organization and recovery foundation for workspace
management. It does **not** enable managed primary placement, write enforcement,
clone provisioning, checkout removal, independent primary hosting, or new agent
tools. Those consumers have separate activation gates. Existing primary sessions,
Startup Context, instruction stores, execution storage and context projection retain
their existing owners and behavior.

## Ownership and identity

`jcode-workspace-types` contains UUID-backed identities and typed requests, views,
reviews and receipts. `jcode-base::workspace::WorkspaceService` owns relationships,
transactions, physical bindings and recovery. `jcode-app-core::workspace` dispatches
trusted same-user client requests without creating an inference Session. No ordinary
agent tool can issue these administrative requests through a workspace tool.
Same-user shell/IPC is not adversarial containment or physical-human attestation.

Projects are named containers. Repositories have explicit logical identity and may
be associated with several projects. Equal names or remote references never merge
identities. Areas are flat and belong to one project. Each checkout/directory has
one project or area home. Standalone locations have no home. Explicit adoption
retains their location ID. Moving a checkout into another project requires its
repository association, optionally included explicitly in the reviewed operation.
Organization operations do not move, rewrite, provision or delete source files.

Existing roots use the [physical-location resolver](PHYSICAL_LOCATIONS.md), including
its UUID/root-witness binding. Canonical aliases return the existing live location
identity rather than changing ownership. Git common-directory identity and Startup
Context/instruction keys remain separate from logical project IDs. Remote metadata
rejects credential-bearing URLs. Credentials remain with Git's existing owners.
Linked-worktree classification compares Git's per-worktree metadata directory with
its common directory. A `.git` file alone is not proof of linked sharing, since
submodules and separate Git directories also use that form.

Archive/unarchive changes presentation only. Active session-index rows keep relevant
archived organization visible in the current view. Retired records retain references.
Only unused, unreferenced container identities can be discarded. Directory retirement
never deletes files. Live checkout retirement is refused in favor of the separately
owned closeout operation. Closed identities have a dedicated query filter.

## Durable state and concurrency

State is under `durable_state_dir()/workspace/catalog.sqlite3`, distinct from the
execution index. An installation marker beside the workspace directory distinguishes
genuine first initialization from missing or damaged initialized data. Ordinary
inspection never interprets unreadable data as an empty workspace. Unknown schemas
and foreign installation identities fail closed. The first public schema is 1.

SQLite owns all database descriptors and uses WAL, full synchronization, foreign
keys and short write transactions. Private files/directories use owner-only
permissions. Typed entity JSON is row authority. Generated indexed columns expose
relationships without creating independently writable copies. Bodies of project
files or conversations do not enter ordinary catalog records.

Organization review records normalized intent, expected catalog revision and any
physical witness. Apply revalidates before committing the complete change and
request receipt together. Same request and normalized intent returns the retained
result. Reusing a request for different intent conflicts. Stale reviews do not
silently apply to newer state. Disconnect or lost acknowledgement does not undo a
committed transaction. Inspect its receipt before retrying.

Kernel-held catalog leases exclude replacement while an operation is using the
catalog. Physical-root leases are keyed by volume/root identity, not pathname or
PID. On Unix their same-user namespace is independent of state-root/TMPDIR aliases.
They coordinate cooperating Jcode owners, not arbitrary external filesystem writers.
Future long-lived consumers must retain their catalog/root leases through their
actual ownership lifetime.

`SessionIndex` is explicitly a projection supplied by a committed Session owner.
It binds session revision, operation identity and placement. Older/conflicting
checkpoints reject. There is no client operation for manufacturing this projection,
and the catalog never writes a Session transcript. Restore marks index rows
unreconciled rather than pretending that old catalog placement relocates live Sessions.

## Backups and restore

Snapshots use SQLite's backup API, not a blind copy of a live WAL file. Publication
uses private staging, fsync, SHA-256, supported-schema and integrity checks. Named
backups are request-idempotent and retained until explicit user cleanup. Automatic
snapshots follow successful organization/import mutation batches and precede import
or restore. The rolling automatic set retains the latest ten. A failed automatic
backup is reported separately from an already committed mutation in its receipt.
Automatic snapshot reclamation has its own exact-target journal, so interruption
between removing a database and its manifest resumes without blocking later backups.

Restore requires a specific reviewed snapshot and current-state fingerprint. It
validates the replacement before touching current authority, acquires exclusive
catalog ownership and rejects pending operations. A healthy pre-restore snapshot and
the exact original database components remain outside replacement. Corrupt originals
are preserved too, without being labeled valid snapshots.

A durable replacement journal precedes renaming originals and publishing the verified
candidate. An interrupted replacement blocks ordinary catalog access. Resume its
exact request/review, rather than initialize an empty database or manually overwrite
files. Stale or unexpected components block recovery. Retry after publication or
receipt persistence recognizes the same result. Old approval reviews are not restored
as executable authorization, and recorded pending effects become recovery-required.
No checkout/filesystem effect is replayed. Physical roots are revalidated, unavailable
ones remain unavailable, and grants cannot activate through incomplete revalidation.

Backups are local private **metadata** backups, not protection against losing the
entire disk and not backups of checkout files, transcripts, retained outputs or
preserved artifact bodies.

## Portable project export/import

Export carries typed organization, repository associations, physical bindings, grant
definitions, closed metadata and session references, plus an explicit external-content
manifest. It excludes credentials, conversation bodies and checkout/preservation
contents. Export paths refer to server-owned storage, not a remote client's filesystem.
Cross-project grant definitions remain complete, source-installation-qualified
external references. Import retains them disabled outside the live grant table until
their external audiences and targets receive explicit binding review.

Import captures and validates the complete manifest, previews identity collisions,
explicit location remapping, offline locations and disabled grant definitions.
Collisions require rejection or deliberate new identities, never name-based merging
or implicit overwrite. New-identity import rewrites typed relationships, not paths
or display text. Physical duplicate ownership still rejects unless explicitly remapped.
Remapped Git roots are resolved through the physical owner. Missing roots are not
created as local stand-ins. Imported session references stay separate from the live
derived Session index. Imported grants are disabled pending their later explicit
audience/target review. This foundation does not claim those grants are enforced.

Apply commits the imported graph in one transaction, with a pre-import snapshot.
Interruption before commit leaves the original graph intact. Request replay after
commit returns the same result rather than importing again.

## Internal client protocol

Before attaching a Session, an authenticated same-user daemon client may send:

```json
{"type":"workspace_probe","id":1}
```

The reply is `workspace_capabilities`, with `catalog_version: 1` and
`managed_rollout: false`. Only catalog semantics are available. Do not infer the
later complete `workspace_v1`/primary-host capabilities from this foundation.

`workspace {id, request}` returns correlated `workspace_response {id, response}`.
Typed operations include status/explicit initialize, paged list, inspect,
organization review/apply, receipt inspection, derived-session reads, named backup,
snapshot listing, project export, import review/apply and restore review/apply.
Initialize acknowledges the original zero-revision creation even on retry. Use
Status for the current catalog revision.
The same adapter works for already attached clients. Lists include total and
revision-bound continuation. The page-size bound is not a member-count ceiling.
An old server must be probed before sending catalog requests.

There is no alternative full workspace CLI or new agent-facing permission tool.
The later basic management client and final SDK reconciliation consume these owners.

## Verification

Focused mechanism and real-store checks are in `workspace::tests` and
`workspace::process_tests`. They exercise relationships, archive/reference behavior,
private modes, WAL snapshots, corruption/foreign schemas, portable collisions/remaps,
disabled imported grants, concurrent writers, kernel ownership, transaction rollback
after actual helper-process exit, and restore/backup publication interruptions.
Fixtures contain synthetic metadata and owned temporary source directories only.

The activated production route is:

```sh
python3 scripts/run_isolated_test.py python3 scripts/test_workspace_catalog.py \
  --binary /absolute/path/to/activated/jcode --artifact-dir /short/private/scratch/path
```

This journey uses actual daemon protocol operations, reconnect and process restart,
checks zero provisional Sessions/model requests, and verifies original fixture files
remain unchanged. It is not final management-TUI or primary-runtime acceptance.
Native physical binding is macOS-supported. Portable DTO compilation does not imply
native volume or service parity on other operating systems.
