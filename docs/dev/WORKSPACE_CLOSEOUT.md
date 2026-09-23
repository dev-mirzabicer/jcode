# Checkout closeout

This service is being integrated in SP-58-C01/WP-07. The current foundation
provides catalog-owned authorization, private inventories, verified
preservation and closing admission/work observation. It does **not** expose checkout removal, a client capability,
a workspace agent tool, or a completed closeout workflow. Ordinary managed
rollout remains unchanged. The rest of WP-07 must integrate current-work gates,
final approval, removal/recovery, retained history and the public adapter before
implementation acceptance.

## Owners and authority

`jcode-base::workspace::closeout` uses the existing workspace catalog,
installation identity, operation/receipt tables and physical bindings. An
initial authorization requires `WorkspaceClientAuthority` and binds one
Location, physical generation, operation and preservation destination. The
conditional-no-loss option defaults off. Agent dispositions do not issue or
broaden this authorization. Revocation is a trusted control, not a field in an
agent judgment. Catalog restoration cannot reactivate a pending historical
closeout authorization.

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
Git LFS inventories the restored history offline. Required payloads are copied
and restored separately with pointer size/SHA-256 verification. Missing payloads
block preservation rather than being treated as preserved pointers.

File dispositions remain separate from Git preservation. An explicit full
archive captures all supported entries. Otherwise file data needs an explicit
disposition. Selected files are copied to private storage and restored to an
independent verification tree. Native macOS metadata copying retains modes,
ACLs and extended attributes. In-tree hardlink relationships and symlink targets
are retained. Source equality is rechecked after copying. A verified existing
preservation reference is exercised as a restore source, not accepted from a
caller-supplied hash.

Preservation completion records an integrity-bound manifest. It is not proof of
current quiescence, approval or removability. Partial artifacts remain outside
the source checkout, and a failed capture does not acquire removal authority.

## Current verification route

The closing fence changes the catalog lifecycle before checking existing work.
New dependent turn/tool admission is refused. Already admitted native operations
retain their shared physical-root leases and may finish. A whole-turn admission
lease is held only until the execution owner's activity record is established,
so a later safe-boundary location move does not pin the old cwd indefinitely.
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

Fixtures use disposable repositories and private catalog/output namespaces.
They are mechanism evidence for the implemented foundation, not acceptance of
R25–R29, final native closeout, the future management TUI or C05 guidance. The
package's final acceptance record must map all requirements to activated
production-path evidence and record failed attempts and platform boundaries.
