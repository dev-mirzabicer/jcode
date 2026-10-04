# Native write-scope boundary

Managed Sessions use the catalog's current scope at native file mutation admission.
Enforcement applies to placed sessions; with `features.managed_primary_launch`
on, unplaced primaries cannot run until placed ([managed placement rollout](../WORKSPACE_ROLLOUT.md)).
Catalog capability negotiation alone does not enable migration.
Placed sessions request access through the [`workspace` tool](../WORKSPACE_AGENT_TOOL.md);
a proposal never changes scope until a trusted client approves it.

## Ownership and invocation

`jcode-base::workspace` resolves ordinary placement roots and explicit grants from
an authoritative Session plus current catalog relationships. `jcode-tool-core`
provides a code-owned `NativeFilePolicy`/`NativeFilePermit` boundary. Neither is a
wire value or a model-supplied approval flag.

The Registry binds the invocation's Session and storage namespace and its immutable
isolated-child restriction. Nested calls retain that binding. Concrete file tools
acquire permits when their effects begin, after any queue or pre-tool hook. Direct
concrete calls use the same admission path. Missing or corrupt Session authority
cannot become an unscoped fallback. A bound policy cannot be retargeted to another
Session ID.

Admission resolves every destination before the first filesystem effect. The
catalog revision and Session binding are rechecked after physical preparation.
No SQL transaction spans file I/O. Permits hold shared physical-root leases and
exclusive per-file mutation leases. Independent files can proceed concurrently,
while root replacement/closeout ownership remains exclusive. Revocation governs
new admissions immediately. A previously admitted native operation can finish,
without retroactively undoing bytes already written.

Ordinary reads are not subject to this new write gate. Placement determines scope,
not the command cwd. Background invocations retain their original cwd but acquire
current authority when effects start.

## Explicit destinations

| Native operation | Complete admission set |
|---|---|
| `write`, `edit`, `multiedit` | The resolved file, including creation of missing parents |
| Unified `patch` | Every actual `FilePatch` destination, plus each deletion entry |
| Codex `apply_patch` | Every add/update/delete destination, both ends of moves, and all removal entries |
| `batch` | Each constituent operation admits independently through its originating Registry |

Patch destination enumeration uses the same parsed operations as execution, not
regular expressions over patch prose. An invalid later destination prevents all
patch writes. This does not promise rollback or patch-wide filesystem atomicity
after an I/O failure. Existing partial-result and cooperative Stop reporting remain.

A symlink write follows its verified, authorized file referent. A removal-only
symlink may reference a file or directory. Unlinking removes the entry and retains
the referent and its contents. Removal admission therefore checks the
entry's directory as well as the resolved target policy. A move that resolves to
the same data file does not remove its source entry. Links cannot confer authority
to mutate an otherwise unauthorized entry directory.

`NativeFilePlan` derives initial regular-file requirements and entry removals from
the ordered parsed operations. This supports delete-then-create without accepting
an initial directory write. A directory reference is pinned for inspection only,
not converted into a generic directory-deletion capability.

## Physical effects

On the supported macOS path, `location::native_files` retains verified directory
handles and uses descriptor-relative no-follow opens, parent creation and unlink.
An existing file is opened without truncation, validated as the admitted regular
file, and only then changed. In-place updates retain inode metadata. Moves preserve
source mode, ACLs and extended attributes through the native metadata-copy owner
before removing the source. Failure reports retained source and prior destination
effects rather than pretending rollback.

Checks cover replaced files/parents, retargeted aliases, missing descendants,
actual filesystem case/Unicode identity, changed volume/device identity and
multiply-linked regular files. Ambiguous or unsupported targets fail explicitly.
Registered nested locations are separate boundaries even beneath a broader root.
Encountered independent Git roots require explicit selection. Git-recorded
superproject relationships distinguish ordinary submodules from unrelated gitfiles.
Inspection does not recursively crawl a project to discover every possible root.

Private harness control state cannot be edited through managed generic file tools.
Scratch exceptions cannot be redirected into control state by a directory alias.
Specialized state, instruction, runtime and capture services retain their own
existing authority instead of borrowing ambient file scope.

## Isolated and external boundaries

An isolated child retains its originating turn's stricter native permission and
path policy. Its own artifact directory remains available under that owner.
Ordinary read-write task destinations also require the original parent's current
workspace scope. A missing parent cannot confer unrestricted task access.
Changing the child's cwd does not create a grant. Original-parent control and MCP
classification/blocklists remain separate, unchanged constraints.

Shell, hooks, MCPs, browser/computer scripts and provider-internal tools do not
become an OS sandbox through this mechanism. Same-user IPC is not physical-human
attestation. Managed primary creation and both primary provider loops reject
internally executing tool routes before inference. This also covers a restored or
switched provider without silently substituting a model. Existing unscoped legacy
route behavior stays separate. Arbitrary external concurrent writers and hardlink
creation are not claimed adversarially contained.

Managed physical workspace support is platform-qualified. Non-macOS unscoped
manual callers retain their compatibility file path and do not acquire a claim of
C01 native enforcement or full ACL/xattr preservation. A managed caller cannot
select that compatibility path to bypass unsupported physical scope.

## Verification routes

Use the isolated test runner and coordinated selfdev owner:

- Base `location::native_files::` exercises actual file descriptors, aliases,
  hardlinks, replacement, parent creation, metadata and entry-versus-referent effects.
- Base `workspace::permission_tests::native_admission_rejects_root_replacement_between_validation_and_pinning`
  faults the exact policy-to-pinning boundary on an owned root.
- App-core `tool::tests::native_scope::` exercises actual Registry aliases, direct
  native tools, all five mutators, patch pre-admission, parallel batch calls,
  grants/revocation, in-flight leases, nested roots and protected state.
- The same family covers actual recursive Git submodules, changed Git indirection,
  an awaited external pre-tool hook across revocation, cross-root ordinary reads,
  and frozen child permissions against current parent scope and artifact ownership.
- App-core `agent::tools::scope_provider_tests::` exercises both actual provider
  loops with an opaque tool route, zero inference and unchanged history.
- App-core `tool::apply_patch::` preserves transformation, deletion protection,
  partial-output and Stop behavior.

These are mechanism and production-boundary fixtures, not prompt-text tests or a
model benchmark. Package acceptance additionally requires the complete caller,
child, migration, notice, provider-route and activated-runtime evidence described
by its acceptance ledger. Passing this focused slice is not full package acceptance.
