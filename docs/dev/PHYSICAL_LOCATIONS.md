# Physical location identity

Use this reference when changing physical project discovery, workspace volume binding,
or the archive-volume adapter. Logical workspace organization and filesystem effects
have separate owners. The new workspace-facing APIs are internal foundations, not a
user-facing catalog, clone command, permission system, or management UI.

## Owners and compatibility

`jcode-base::location::resolve_project` returns `ProjectFacts`: the canonical active
root and the existing physical `ProjectKey`. Startup Context re-exports these as its
original `ActiveProject` and `ProjectKey` names and translates resolution errors to
its existing typed startup failure. Instruction repositories call the shared resolver
directly. Neither caller initializes another domain merely to obtain physical facts.

Git identity remains the canonical Git common directory. Linked worktrees share that
key while capturing files and selecting instruction configuration from their own active
root. Non-Git identity remains the canonical launch directory, not its parent. An
independent clone has a different key even when remotes and commit history match.

The legacy digest input is unchanged: `git` or `directory`, a NUL byte, then the
canonical UTF-8 path. Stored Startup Context identity, plan schema, private filenames,
instruction `project-<digest>` IDs, and private external-instruction checkout paths are
unchanged. This extraction performs no migration or copying of plans, captured bodies,
instruction stores, or Session messages. Frozen instructions, provider prefixes,
context projection and resume behavior stay with their established owners.

## Volume and root bindings

`location::volume::LocationResolver` owns read-only native volume discovery and binding.
On macOS it obtains actual mount facts from `statfs`/`getfsstat`, then verifies
`diskutil`'s VolumeUUID and mount information. Labels, internal/external classification,
read-only state and free bytes are observations, not identity or capacity guarantees.
Ambiguous UUIDs and unavailable or unidentifiable volumes fail explicitly.

`PhysicalBinding` stores the volume UUID, volume-relative path, original observed path,
root witness and binding generation. A witness contains inode and birth time. A live
file descriptor plus device comparison protects the resolution observation from ordinary
path replacement. The device number is deliberately not durable identity across remounts.
On APFS, `/Users` and similar Data-volume firmlinks are matched to a mount-relative
spelling only after verifying that both spellings name the same root witness.

Resolution can find the same UUID at a new mount and returns `relocated: true` with the
new path. It never changes a Session cwd. A replaced root requires explicit reviewed
rebind at the higher operation owner. Missing mounts and old paths are never created.
Bindings do not change existing Startup Context keys after an external relocation.

`PathBinding` represents an existing witnessed ancestor plus a missing relative suffix.
It supports saved default bases that do not exist yet. Existing aliases are canonicalized
before binding. A dangling link is an error. A later symlink, file or different mounted
volume in a previously missing suffix invalidates resolution instead of redirecting it.
Serialized bindings are validated again when resolved, including path and UUID checks.

## Checkout destination review

`checkout_destination` takes a selected volume UUID and either a custom absolute path
or explicit project/checkout filesystem components. The defaults are:

- Internal volume: `<home>/jcode-checkouts/<project>/<checkout>`.
- External volume: `<verified-mount>/jcode-checkouts/<project>/<checkout>`.
- Saved volume base: the explicitly bound base followed by the two components.

Custom paths and saved bases must resolve to the selected volume. If home is on a
different volume, an internal default fails rather than silently changing the selection.
Names must be single filesystem components. Existing destinations require a separate
adoption operation. Read-only volumes reject destination review.

These operations have no filesystem creation or deletion effects. A returned binding is
not a permission grant, a reservation or an execution lease. The eventual clone owner
must acquire its own operation lease and use `resolve_destination` again at the effect
boundary. Arbitrary external writers, hardlink aliases, forged same-user state and
filesystem replacement after this read-only observation are not an OS sandbox claim.
I/O and capacity failures still require handling by the operation performing effects.

## Archive compatibility

Execution storage consumes the same native UUID/mount verification and free-space
helper. It retains its own `ArchiveConfig`, directory handles, serialized device/inode
witnesses, descriptor-relative creation, retention, journals, output aliases, read paths
and recovery. Its existing Linux UUID-device verification is retained. No archive
configuration, ownership, encryption, reserve or retention policy changes here.

Workspace native volume discovery/binding is supported on macOS. Other platforms return
an explicit unsupported workspace-volume result. The existing cross-platform project
resolver remains available independently. Linux archive compatibility is not a claim
of native Linux workspace discovery, and portable compilation is not platform acceptance.

## Verification

The Startup Context `physical_identity_*` fixtures were run before and after extraction.
They compare exact digest inputs and serialized identity, instruction-store IDs and
specificity, and real active-root capture across Git, non-Git, subdirectory, linked
worktree and symlink fixtures. Bare and damaged Git identities fail. Red/green tests
cover dangling or shadowed `.git` markers and ambient `GIT_DIR`/`GIT_WORK_TREE`
redirection. The resolver preserves ordinary Git trust configuration but excludes
process-local repository, index, object and discovery-path redirection so the supplied
directory remains the physical source of truth.

`location::volume::tests` covers defaults/custom/saved bases, binding round trips,
wrong/offline/duplicate/read-only volumes, mount rename, root replacement, dangling and
late symlinks, nested mounts and malformed input. Fault-volume tests use synthetic mount
facts with real disposable directories. Native macOS tests inspect actual internal and
read-only volumes. The explicitly selected `native_external_volume_fixture` requires
`JCODE_LOCATION_FIXTURE_MOUNT` and `JCODE_LOCATION_FIXTURE_UUID`, writes and removes only
its newly owned tempfile directory, and creates no checkout-default directories.

Archive regression uses the existing `native_archive_fixture_uses_verified_volume_and_stable_aliases`
fixture and its explicit isolated `JCODE_EXECUTION_ARCHIVE_FIXTURE` config. It verifies
real archive placement, relocation, continuation and retained media without touching
production output bundles. See [execution storage](EXECUTION_STORAGE.md) for that owner.

The activated daemon journey uses the existing editor/creation protocol, not private
plan writes. It saves a main-checkout selection, captures distinct linked-worktree
contents through subdirectory and alias launches, verifies independent clone/non-Git
defaults, and resumes the original immutable capture after source edits:

```sh
python3 scripts/run_isolated_test.py python3 scripts/test_physical_locations.py \
  --binary /absolute/path/to/activated/jcode --artifact-dir /short/private/scratch/path
```

The short artifact path keeps Unix socket names within the macOS limit. The fixture
records all events and zero model requests and tears down only its owned daemon.
