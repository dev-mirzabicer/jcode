# Checkout closeout

This service is being integrated in SP-58-C01/WP-07. The current foundation
provides catalog-owned authorization, private inventories, verified
preservation, closing admission/work observation, review-bound final approval,
and catalog-journaled removal with retained history. The native workspace
protocol now supplies the trusted macOS control path through the shared execution
supervisor. It has been exercised with owned disposable fixtures, not real user
checkout deletion. Harness API v1.10 and both SDKs consume that same service;
remaining caller/admission and preservation cases, final combined runtime
verification and activation remain WP-07 work.
There is no workspace agent tool or ordinary managed-rollout activation here.

## Native control and retained execution

`workspace_probe` advertises optional `closeout_version=2` on macOS. Its absence
means unsupported, not an empty closeout list. `WorkspaceRequest::Closeout`
contains the typed `CloseoutRequest`; controls do not require creating or
attaching a primary Session. The same authenticated same-user boundary applies,
not physical-human attestation or arbitrary-shell containment.

Begin records the exact initial human authorization. Long refresh, disposition,
preservation, review, approval, recovery and finish operations use Execute with a
caller-held request UUID, operation ID, expected revision and typed action. An
action receipt identifies its retained execution run. Reusing a request with the
same intent returns the original action; different intent conflicts. A new
attempt requires a new request identity and current reviewed state. No schema
field lets a caller manufacture trusted provenance or waive preservation.

The action journal records immutable domain outcomes. It does not duplicate
execution state: an applied domain result, a sealed output, and a terminal run
are separate facts. InspectAction and Execution inspection expose those owners.
Storage/control failure names the original action and run rather than starting
another attempt. Output/status/Stop requests are restricted to that exact
action's invocation. Force, arbitrary run listing and command-survival controls
are not closeout operations.

The existing execution supervisor owns the producer, Stop signal and capture.
Blocking filesystem work runs in its retained blocking task, independent of
client waiting. Background acceptance precedes completion. Quiet Git/process
inspection observes Stop and drains/reaps owned processes; inventory and archive
streaming observe it at bounded chunks/entries. Capture still retains partial
and terminal evidence after Stop. Cooperative Stop does not abort the waiter
and falsely claim that its filesystem worker has stopped.

After preservation, inspect the new removal review and its issues, approve that
exact review through the trusted path, then explicitly request Finish. Neither
an action receipt nor preservation alone authorizes removal. Review and recovery
results retain their revision/target identity. Existing human management and
final agent-assisted guidance remain the later C01/C04/C05 consumers.

## Domain authority

The curated Harness route advertises `checkout_closeout_v2`, then probes the
actual daemon's optional closeout version. `JcodeClient::closeout` in Rust and
`JcodeClient.closeout` in TypeScript preserve complete typed request/reply
contracts without Session attachment. The bridge keeps native correlation IDs
separate from client frame IDs. Bridge and clients reject foreign target/kind
replies, and exact output-part identity includes part, offset and expected
digest. TypeScript snapshots caller intent before asynchronous negotiation.
Domain rejections remain typed `CloseoutReply::Rejected` values, distinct from
transport or reply-correlation failures. The common synthetic wire
matrix checks both language implementations, not model behavior or prose.

Catalog organization/discovery still has its existing native owner. This
curated closeout API does not add a second catalog or a full workspace CLI.

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

Closeout version 2 keeps ordinary paths as JSON strings and represents an opaque
Unix path as `{ "unix_bytes": [47, 255] }`. This applies to inventory paths/link
targets, retained files, dispositions and removal progress. Rust retains PathBuf
identity internally; TypeScript exposes `CloseoutFilesystemPath`. Do not coerce
the byte variant to text for filesystem operations. NUL, unknown fields, invalid
byte values and noncanonical byte encoding of Unicode paths are rejected.
Older bridge/native versions are refused before SDK control submission rather
than silently losing path identity. Ordinary persisted path strings remain valid.

Native APFS rejects non-UTF-8 filenames but permits opaque symlink targets. The
native fixtures exercise those targets, newline/tab names and a nested Git root
containing newline/quote characters through preservation and removal. The wire
codec separately tests distinct opaque filename bytes. This is not a claim that
APFS accepts filenames its kernel rejects or that another filesystem was tested.
Git metadata paths are read one framed result at a time, and temporary alternate
paths use Git's quoted-byte syntax rather than newline splitting or lossy text.

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
Git's own dangling-root graph inspection also retains otherwise unreferenced
commits, trees, blobs and tags, including objects named only by pseudorefs or
unfinished-operation metadata. It does not classify prose or edit source refs.
An unborn symbolic branch is distinguished from a damaged existing ref even
when other branches remain. Git-inventory receipts predating this graph capture
must be refreshed before new preservation can be authorized.
Staged and conflict-index blobs are retained through an isolated preservation
commit. Original index, split-index and per-worktree administration are separately
copied and restored, including metadata outside a linked worktree's directory.
The production verifier loads the restored index and compares its complete
entries before accepting the capture. Source indexes are not converted: Git's
private expanded index supports observation with the original repository's
configuration, with split/sparse index representation disabled only for the
private view. Otherwise Git can compact the view and freshen source tree objects
during nominal inspection. Cache preparation precedes immutable filesystem
witnesses; changed source witnesses are not ignored to accommodate Git.
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
caller-supplied hash. Its verified restoration remains in the preservation store
with metadata copied from the original source, not assumed present in the external
reference. Later loss of that external copy cannot invalidate the retained copy.

The final file manifest is sealed after directory metadata and hardlinks finish.
It records integrity fingerprints of the retained modes, ownership, birth/modified
times, flags, ACLs and extended attributes. Resource-fork verification is streamed
through the native positioned-read API. Inode, access time and ctime are not
metadata-content fingerprints. Final approval/removal rechecks these receipts as
well as file bytes, so metadata-only corruption is not mistaken for a valid copy.
Older captures missing metadata receipts require fresh preservation, not an
implicit upgrade of old removal authority.

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

The same read-lifetime owner covers local submodule and LFS materialization.
Git resolves relative submodule URLs without changing source configuration;
the admitted endpoint, not merely its relative spelling, selects physical
ownership. LFS's reported local media tree is protected too. Closing blocks
new transfers, while admitted readers finish normally. Source trust remains
operation/stage-bound and separate from these lifetime leases. No checkout
is removed or existing reader cancelled just to make a clone proceed.

## Specialized source readers

Startup Context capture and receipt observation retain their original facade's
durable-state namespace. Each stable-file attempt holds location use for the
logical and resolved source through its complete read and validation. Closing
is an unreadable-source/blocking outcome through the existing startup owner,
not permission to rewrite or recapture an earlier authoritative snapshot.
Saved selections and custom selections use that same boundary.

Delegation guards the host-resolved catalog/creation cwd and uses its retained
Startup Context owner. An ancestor cwd is not made into a recursive discovery
target merely to inspect a catalog. Gmail draft/send guards its documented
absolute attachment paths, including aliases, before any external request.
Neither guard grants a new read permission or changes child authority.

Managed instruction and external AGENTS body reads retain source-namespace
identity. Managed discovery holds one permit through the complete source-tree
read. A Closing scope remains an explicit resolution error, including when
optional project-addendum applicability cannot be read. It is not an empty
catalog or permission to omit project guidance from an explicit global agent.
Independent explicit global resources remain readable. Direct repository file
reads and existing repository mutation/setup/
draft-operation leases participate in checkout use. Previously frozen Session
instructions remain unchanged; failed source reads use the existing diagnostic
and invalid-candidate mechanisms instead of silently activating another source.
These are specific native owner boundaries, not an OS sandbox or a claim that
arbitrary shell, browser, computer, MCP or third-party reads are contained.

External skill invocation and reload retain the registry's source namespace.
Discovery, first-run compatibility import and package-copy/capture readers
participate in location use. A refused discovery produces a catalog diagnostic,
not a successful empty selection or an implicit lower-priority invocation.
Skill-list results expose the diagnostic facts. Managed package capture and
external Copy hold the source/destination trees while reading their content and
metadata. A completed Copy can still replay its existing receipt without reading
its now-unavailable original source. Earlier active text and captured draft
bytes remain intact. This does not change skill delivery, source precedence,
the exclusive active-skill mechanism or the later C05 corpus/migration.

## Journaled removal and retained history

Inventory also binds source metadata content for every removable entry. Retry
checks that receipt for captured slots, regular files, hardlinks and remaining
directories, rather than accepting newly observed metadata merely because its
inode is unchanged. Directory mtime is excluded from this destructive receipt
because removing children changes it; ACLs, xattrs, flags, ownership, mode and
birth time are still bound. Full preservation receipts continue to retain the
original directory timestamps. Older inventories without source receipts require
fresh preparation while intact or non-destructive retention after partial work.

Registration, rebind and adoption cannot overlap a Closing root or its owned
quarantine/holding paths, including application of a prior review. Every resumed
destructive attempt also inspects current catalog/Session/linked references at
all those roots. Newly linked data needs an actual preserved inventory entry;
an intact old reference-file digest alone is not current authorization.

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

Missing or corrupt historical reference evidence remains a blocker to removal.
It does not strand the trusted **Unregister and retain files** alternative.
That review exposes `evidence_issues` separately from blocking `issues`, and
binds the observed evidence path, expected/actual digest and physical witness.
Changing the damaged evidence invalidates Apply too. The Retained outcome and
report preserve the warning and observation without claiming complete evidence
or performing another unlink. Current catalog/physical errors and unknown or
live work remain blockers; this is not a force-removal override.

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

Removal intent retains its own immutable, digest-checked reference snapshot,
matched to the current approved review before quarantine. Original inventory
and preservation references remain untouched. Catalog-only resolution of a
nested checkout therefore cannot be undone by replaying its historical blocker
after an interruption. The removal report links both evidence boundaries.
Corruption blocks further effects. An older journal without this snapshot may
capture it only while its complete original source and review can still be
revalidated. Once that source is gone, it cannot invent the missing evidence;
trusted retain-files recovery remains available instead of guessed removal.

A restored older catalog cannot manufacture receipts for later filesystem
effects. If those effects cannot be reconciled, resumed removal stays blocked;
the trusted retain-files alternative remains available without deleting data.
The ordinary agent capability remains gated; native and Harness/SDK trusted
clients use these same operations. Final combined and activated-runtime
verification remains part of the package's integration boundary.

Closed is published only after the original/quarantine paths are absent and the
owned holding directory is empty and removed. `closed_checkout_history` retains
the Location, operation, preservation references and private physical-removal
report. The catalog owns lifecycle truth; the report records physical evidence.
Portable imports may retain external history references without inventing a
local operation. Session and execution owners retain transcripts and outputs,
and missing-cwd repair remains explicit.

## Current verification route

`scripts/test_workspace_closeout.py` runs the actual daemon and standalone
Harness bridge in an owned namespace through `scripts/run_isolated_test.py`.
Pass an explicit immutable TUI `--binary`, a short owned `--artifact-dir`, and
`JCODE_WP07_BRIDGE`/`JCODE_WP07_RUST_PROBE` pointing to the candidate bridge and
the compiled opt-in `jcode-sdk` integration test `closeout_native`. Its TypeScript
client detaches during real running work, reconnects, stops and explicitly
retries, then preserves, reviews, approves, removes and inspects its disposable
checkout. Rust reads the same retained history through its real SDK. Teardown
checks terminal executions before ending the owned daemon/bridge. Artifacts
record identities, complete/failed outcomes and cleanup; a passing fixture is
not a claim that the user's shared runtime was activated.

`scripts/test_workspace_closeout_retention.py` exercises an intentionally
created primary, native command output and a linked side-panel document through
the actual daemon. It enables managed launch only in its owned fixture config.
`JCODE_WP07_SESSION_PROBE` names the compiled opt-in base integration test
`closeout_retention_native`, which captures the complete Session plus journal
through `Session::capture_readonly` and reads output through the existing
execution inspector. It verifies prior messages and frozen content unchanged,
allowing only the existing structurally identified non-waking scope notice and
its checkpoint metadata. It then verifies failed continuation retains the
original input, explicit cwd repair preserves historical context without
inference or directory resurrection, and a new input uses the selected cwd.
The provider is scripted localhost HTTP, not a paid model or behavioral test.
Both successful and failed fixture outcomes retain cleanup and binary evidence.

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
