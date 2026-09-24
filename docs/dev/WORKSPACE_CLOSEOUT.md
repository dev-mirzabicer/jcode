# Checkout closeout

This service is being integrated in SP-58-C01/WP-07. The current foundation
provides catalog-owned authorization, private inventories, verified
preservation, closing admission/work observation, review-bound final approval,
and catalog-journaled removal with retained history. The removal core has been
exercised only through owned disposable fixtures. It is **not** yet exposed as a
client capability or workspace agent tool. Ordinary managed rollout remains
unchanged. Complete caller/admission coverage, public adapters and final runtime
integration/activation remain WP-07 work before implementation acceptance.

## Owners and authority

`jcode-base::workspace::closeout` uses the existing workspace catalog,
installation identity, operation/receipt tables and physical bindings. An
initial authorization requires `WorkspaceClientAuthority` and binds one
Location, physical generation, operation and preservation destination. The
conditional-no-loss option defaults off. Agent dispositions do not issue or
broaden this authorization. Revocation is a trusted control, not a field in an
agent judgment. Catalog restoration cannot reactivate a pending historical
closeout authorization.

Final review keeps the root Closing and rechecks actual retained files, Git/LFS
payloads, administration, current references and live-work observations. A
trusted client may approve that exact operation/review pair. An agent's no-loss
declaration can authorize only an operation whose initial human approval enabled
the conditional option. Neither route can waive structural findings or create
the initial human grant. Typed provenance distinguishes the two routes, request
IDs make retry idempotent, and stale target/revision/evidence is rejected.

Restoring the catalog clears pending closeout authority, including an operation
that was already interrupted before the snapshot. Historical closed receipts are
not turned into new authority. A review committed before automatic-backup failure
remains inspectable and reports the partial outcome rather than pretending the
review was rolled back. The removal owner retains physical ownership and re-runs
the same checks at its destructive boundary.

Preparation does not change Session history, stop an active task, retire a
Location or delete checkout files. No historical cwd is rewritten. The native
same-user boundary remains a cooperative harness boundary, not physical-human
attestation or an OS sandbox.

## Inventory and preservation

Inventories are immutable private JSONL files, with digests, stable entry IDs,
physical witnesses and bounded metadata pages. File content is streamed through
a fixed-size buffer. Symlinks are recorded without following their targets,
mount/special-file findings block preservation, and changed source entries
invalidate the observation. Dispositions identify exact entries and provenance.
Unknown data is not implicitly classified as disposable.

Refresh also snapshots organization/grants, related Session placement and cwd,
original child-parent identity, side-panel links, child artifacts and instruction
repository references. These use metadata-only owner APIs, not transcript or
Markdown hydration. Harness-state containment is rejected before inventory;
independently registered nested locations remain explicit findings. History and
side-panel state are not rewritten by this inspection.
Instruction dependencies use a dedicated projection of configured stores and
existing conventional candidates, not active instruction selection or Git index
reads. The ordinary instruction activation/resolution policy is unchanged.
Known linked data cannot rely on a redundant-data disposition when removal is
reviewed. Other worktrees depending on Git metadata inside the target block
approval, even if preservation otherwise succeeded.

Complete refresh additionally records offline Git status/index, refs, reflogs,
worktree/submodule sharing and metadata locations, including nested repositories.
Git commands reuse the hardened checkout command constructor. Their owned process
groups and complete output use the existing execution capture owner. Refresh
and preservation never modify source refs, run setup hooks, fetch from a remote,
or infer retention from a clean status or a merged branch.

History preservation creates temporary private refs for original refs, detached
HEAD and reflog tips. It writes a bundle, restores it into a separate bare
repository without alternates, checks exact refs and runs Git integrity checks.
The acquired refs from clone provisioning remain ordinary preserved source refs.
Staged and conflict-index blobs are retained through an isolated preservation
commit. Original index, split-index and per-worktree administration are separately
copied and restored, including metadata outside a linked worktree's directory.
The production verifier loads the restored index and compares its complete
entries before accepting the capture. Source indexes are not converted: Git's
private expanded index supports observation with the original repository's
configuration. Cache preparation precedes immutable filesystem witnesses.
Initialized nested repositories are inspected individually, including their
submodule relationships, without letting parent status refresh child indexes.
Git LFS inventories the restored history offline. Required payloads are copied
and restored separately with pointer size/SHA-256 verification. Missing payloads
block preservation rather than being treated as preserved pointers.

File dispositions remain separate from Git preservation. An explicit full
archive captures all supported entries. Otherwise file data needs an explicit
disposition. Selected files are copied to the chosen storage and restored to an
independent verification tree. Native macOS metadata copying retains modes,
ACLs and extended attributes. In-tree hardlink relationships and symlink targets
are retained. Source equality is rechecked after copying. A verified existing
preservation reference is exercised as a restore source, not accepted from a
caller-supplied hash.

Preservation writes traverse held directory descriptors, create files and links
relative to those handles, and verify retained ancestors. A missing or replaced
destination cannot become a recursive fallback mkdir. Git writes use a pinned
working directory and relative artifact destinations. These safeguards do not
claim adversarial containment of arbitrary same-user filesystem changes.

Containment directories and metadata files request owner-only permissions where
supported; preserved entries retain restoration metadata. The record exposes
the selected filesystem's actual ownership-enforcement state, including disabled
ownership on an external volume. Destination selection does not require a second
privacy gate and does not change drive settings. This is ordinary personal-use
protection, not encryption or a promise of stronger filesystem enforcement.

Preservation completion records an integrity-bound manifest. It is not proof of
current quiescence, approval or removability. Partial artifacts remain outside
the source checkout, and a failed capture does not acquire removal authority.

## Activity and source ownership

Live Session activity has a private same-user projection under
`/tmp/jcode-session-activity-<uid>`. Its kernel lock lasts with the existing
activity guard. Inspection verifies the original execution namespace and token,
then reads current Session metadata without initializing a foreign store.
Committed cwd changes do not require rewriting this projection. Dead process
ownership is not inferred from a PID. This supplements the local pending-input,
execution and external-process observations rather than replacing their owners.
Full caller integration remains part of the staged acceptance boundary above.

Native mutations cannot edit that projection or shared physical-root lease
metadata, including through aliases. Shell/external effects remain outside this
native enforcement. Local and file-transport clone acquisition checks Closing
admission and retains physical source use while Git reads it. Parent creation
and publication retain shared parent ownership with exclusive child creation
and no-replacement publication, allowing ordinary parent readers.

## Journaled removal and retained history

`finish_closeout` consumes an existing current authorization, not a force flag.
It rechecks authority after acquiring physical/catalog ownership, records intent
before effects and exclusively renames the checkout to a same-volume quarantine.
An unexpected root or replacement at the original name is retained and reported.
The quarantine and holding paths remain protected by the Closing admission owner.

Entries are processed sequentially in reverse inventory order with bounded-memory
JSONL reading. Each entry has a durable intent, is captured into an exclusive
holding slot, verified there, then unlinked through retained directory handles.
Symlink targets are not followed. Directory removal must be empty and on the
same filesystem. There is no recursive deletion of an unverified path or
reset/stash/clean fallback. Late data or incomplete observations leave a partial
result that can be inspected and reconciled.

Linked-worktree registration is retired through ordinary Git worktree removal
after quarantine and preservation of its administration. Its separate
intent/receipt reconciles interruption without deleting shared Git directories
or forcing removal. Sibling files and shared refs remain outside the target.

`closeout_removal_progress` supplies revision-bound pages of NotProcessed,
Unconfirmed and Removed entries. An effect without its final receipt stays
Unconfirmed even if an unlink may already have happened. Pre-removal revocation
cannot reopen an operation that has started removing data. Its trusted recovery
review is a separate operation, not ordinary revocation.

Removal observes its existing execution's durable Stop cause before destructive
admission and between journaled entries. An admitted entry finishes its receipt,
then Stop retains partial progress for a new, separately identified attempt.
The production adapter must own the blocking producer independently of client
waiting and keep inspection/control responsive. Stop is not new deletion
authority, rollback, or permission to reuse an old terminal execution.

## Trusted recovery and retaining files

`review_closeout_recovery` requires trusted client authority. It fences the
location and clears earlier final and conditional authorization before collecting
current findings. `pending_closeout_recovery` finds a committed current review
after lost delivery. `apply_closeout_recovery` binds a request ID to that exact
operation/review, rechecks physical observations, references and live work, and
records the trusted decision. A committed fence or backup failure is not reported
as a rolled-back operation.

- Restart preparation requires the intact original root and no evidence that
  removal began. Old inventories and preservation references remain in review
  history, but they no longer authorize the restarted operation.
- Resume removal checks preserved data and every remaining or pending entry
  against the retained journal. Unknown disappearances, replacements and newly
  arrived entries block it. A precisely inspected empty holding directory whose
  creation receipt was interrupted can be adopted only by this fresh human
  decision. It is never adopted automatically from its name.
- Unregister and retain files performs no checkout, quarantine, holding-directory
  or shared Git deletion. It ends the registration and retains inspectable paths,
  reports and earlier evidence. The Location is `unregistered`, and the operation
  is `retained`, not `closed`. This does not claim that missing data was preserved
  or that an old unconfirmed effect completed. Re-adoption is an explicit new
  registration, with no inherited authority from the old identity.

Closed-history queries include both historical outcomes with their distinct
typed states. Export/import retains their external references without inventing
a local operation or reenabling grants. After quarantine, instruction references
come from the integrity-checked pre-removal snapshot and are labeled recorded,
not newly resolved from a missing cwd. Current Session, catalog and side-panel
references remain independently observed.

A restored older catalog cannot manufacture receipts for later filesystem
effects. If those effects cannot be reconciled, resumed removal stays blocked;
the trusted retain-files alternative remains available without deleting data.
The ordinary agent capability and public client integration remain gated.

Closed is published only after the original/quarantine paths are absent and the
owned holding directory is empty and removed. `closed_checkout_history` retains
the Location, operation, preservation references and private physical-removal
report. The catalog owns lifecycle truth; the report records physical evidence.
Portable imports may retain external history references without inventing a
local operation. Session and execution owners retain transcripts and outputs,
and missing-cwd repair remains explicit.

## Current verification route

The closing fence changes the catalog lifecycle before checking existing work.
New dependent turn/tool admission is refused. Already admitted native operations
retain their shared physical-root leases and may finish. A whole-turn admission
lease is held only until the execution owner's activity record is established,
so a later safe-boundary location move does not pin the old cwd indefinitely.
Location preparation likewise holds a shared physical lease, so a second primary
can be prepared in a busy Ready checkout while relocation/removal remains blocked.
The actual tool producer, independent native worker and detached observer hook
hold their own use leases through their work, not merely until a foreground
waiter returns. This adds lifetime safety, not a new read-access grant or shell
sandbox.

Work observation reads existing execution ownership, exact native-command cwd,
Session activity, pending inputs/location controls and catalog references. It
also retains actual macOS `lsof` observations of open files/directories. Unknown
or incomplete observation is a blocker, not idle. The executor's own cwd is
explicitly reported. Preparation never kills these owners. These observations
are not final approval or a guarantee against a later arbitrary external writer.
Final destructive integration must revalidate them under exclusive ownership.

Run through the coordinated selfdev test owner:

```text
scripts/dev_cargo.sh test --profile selfdev -p jcode-base --lib workspace::closeout::tests:: -- --test-threads=1
scripts/dev_cargo.sh clippy --profile selfdev -p jcode-base -p jcode-workspace-types --lib -- -D warnings
```

The external-volume fixture is opt-in through `JCODE_WP07_EXTERNAL_MOUNT`,
`JCODE_WP07_EXPECTED_VOLUME_UUID` and `JCODE_WP07_EXPECTED_OWNERSHIP`. It checks the
independently supplied identity before creating an owned temporary directory,
exercises preservation/restoration and verifies cleanup. An unselected fixture
does not establish external-volume acceptance.

Fixtures use disposable repositories and private catalog/output namespaces.
They are mechanism evidence for the implemented foundation, not acceptance of
R25–R29, final native closeout, the future management TUI or C05 guidance. The
package's final acceptance record must map all requirements to activated
production-path evidence and record failed attempts and platform boundaries.
