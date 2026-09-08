# Instruction stores

**Status:** Phase 3 Git repository infrastructure, primary activation, versioned seed adoption, and the [instruction manager](INSTRUCTION_MANAGER.md) with reviewed editing and explicit repository controls.

Jcode has a typed service for Git-versioned managed instructions. App-core `Server` owns the service. Server construction performs no repository I/O; the first primary instruction activation initializes or validates the global store and then uses the same service for activation, clear, and transfer.

## Locations

### Global

```text
~/.jcode/instructions/
```

The global store is an owner-only standalone Git repository. It uses local branch `main` by default. The rest of `~/.jcode` is not part of this repository.

### Git project submodule

The conventional Git-project store is:

```text
<project>/.jcode/instructions/
```

It is a true submodule. Jcode can commit inside the instruction repository, but never commits the parent project's `.gitmodules` or gitlink update. Parent-project commit timing remains under project-owner control.

### Project external repository

A project can explicitly configure either:

- A repository URL and branch cloned under private Jcode state
- An existing local checkout path

### Non-Git project

A non-Git project can use a standalone project-scoped Git repository, conventionally under `.jcode/instructions`. This does not make the whole project a Git repository.

## Project configuration

Explicit configuration lives at:

```text
<project>/.jcode/instructions.toml
```

It is schema-versioned. Modes are `submodule`, `external-remote`, `external-local`, and `standalone`. Invalid explicit configuration, including a dangling or ordinary configuration symlink, fails visibly. Jcode does not silently ignore it and continue as global-only.

## Runtime authority

- A present working file is authoritative, even before commit.
- An intentionally empty resource body remains valid where its consumer permits
  it. Keep the required frontmatter. A blank manifest or roster is not a valid
  empty resource.
- A present invalid file fails the affected runtime operation rather than falling back to another scope.
- A missing committed file is read from current Git `HEAD` only when the caller explicitly requests the accepted fallback policy.
- Managed reads and writes reject path traversal and symlink escape.

## Saving and Git history

Repository mutations use:

- One cross-process mutation lease per instruction repository, including initialization and repository setup
- Draft base `HEAD` plus exact target fingerprints
- Atomic same-directory file replacement
- An isolated Git index that stages only owned paths
- One local commit per real change
- Structural operation identities for idempotent retry
- Runtime resource and dependency validation before commit publication

Affected manifest, resource, and dependency behavior is compared with the expected committed `HEAD`. New manifest, resource, reference, or dependency errors block the commit and leave the working edit visible for repair, including on retry. Existing unrelated resource errors do not block a valid edit, and multi-path operations can repair references atomically. Unrelated staged, dirty, and untracked repository state is preserved. Detached repositories cannot Save until a branch is selected. Restore validates complete UTF-8 before changing the working file and writes historical content as a new commit. No-op Save and restore create no commit.

Submodule, external-checkout, and standalone setup also carry operation identities. A retry reuses the repository already created by an interrupted setup while the same repository lease prevents concurrent setup from racing it. Existing checkout reuse verifies the requested origin, attached branch, manifest, resources, and dependency graph before project configuration is published.

Ordinary operations do not offer reset, rebase, force push, commit deletion, or history rewrite.

A scoped Save cannot bypass a pending merge, rebase, cherry-pick or revert.
This remains true after conflict resolutions have been staged. Finish or abort
that Git transaction explicitly before saving instructions. Rejection preserves
the working source, index and HEAD. Recognizing an already completed Save still
returns its existing receipt without replaying it.

Git commands bind to the verified instruction worktree and its exact Git
directory. An instruction directory nested inside a different repository is not
adopted as that repository, including when a submodule checkout is damaged.
Ambient `GIT_DIR`, `GIT_WORK_TREE`, index and object-directory variables cannot
redirect the operation. Explicit isolated-index arguments remain supported.

Repository hooks and filesystem-monitor commands are disabled. Configured
clean/smudge/process filters are neutralized, and custom merge drivers fail closed
instead of running repository-provided commands. Owned files are staged as exact
Git blobs, preserving complete bytes rather than applying attribute conversion.
This is literal-file versioning, not support for Git extensions that depend on
executing filters. Git credential tooling remains responsible for authentication
during an explicitly requested network operation.

Mutation and attached-draft kernel locks live in the actual Git common directory,
so configuration aliases and separate Jcode state directories cannot admit two
writers to the same checkout. Stable setup coordination protects initialization
and project configuration before that Git directory exists. Draft recovery
records remain in private application state. A live draft blocks branch changes
even when the other client uses a different project alias or state directory.

Private remote clones are completed in an owned staging directory before the
checkout path is published. Submodule setup retains Git-native relative-URL
resolution against the parent remote and leaves parent changes uncommitted.
Existing local attachments require their own Git worktree and committed baseline.
An unmanaged unborn Git directory is not overwritten by seed initialization.

Completed-operation recovery matches the exact operation trailer, not an ID
prefix or incidental commit-subject text. Recognizing a published Save does not
refresh the ordinary index again: the user may have staged newer target content
since that Save. If interruption left an index/worktree difference, inspection
shows it rather than silently resetting it during a retry.

### Draft service

The Rust repository service provides `InstructionDraftWorkspace` for client-owned
unsaved intent. The full-screen manager uses this service for reviewed editing. Draft records live in private durable state, outside the
instruction repository, and bind session, configured repository, branch, HEAD,
generation and exact captured files. Closing or dropping a workspace releases
its kernel lease without saving or deleting the draft. Explicit discard removes
only its draft record. A branch change refuses while a draft is attached.

`review_commit` captures complete working, committed and proposed file versions
without writing source, HEAD or the index. Validation uses the prospective
committed tree, so an uncommitted dependency cannot make an invalid scoped commit
look valid. Default-agent checks at this review boundary do not change existing
activation error routing. The caller can render previews against that same
private candidate. Review is not activation and is not a filesystem watcher.

Save requires review of the exact generation, rechecks target/branch state,
persists attempt identity, and delegates to the existing scoped Git publisher.
Recovery recognizes a published commit after a lost receipt. Before publication,
only original or exact intended partial-write states can be resumed. Divergent
newer working changes remain untouched. A no-op receipt is durable without a new
commit. Managed source capture rejects nonregular files, including FIFOs without
waiting for a writer on Unix.

## Explicit synchronization

Fetch, pull, push, branch checkout, branch creation, remote configuration, and repository setup are explicit operations. Jcode never automatically pulls or pushes.

Pull requires a clean instruction repository. The manager exposes fast-forward-only
pull and rejects divergence. The lower-level repository API also has an explicit
merge strategy, but the manager does not select it. Conflicts remain visible and
require deliberate Git recovery before further mutation.

## Initialization and recovery

First installation materializes the shipped profile kernel, common layer, Mermaid and available-skills resources, `jcode` compatibility agent, and managed profile-transition notifications together with eligible exact legacy imports, validates the manifest plus complete resource and dependency graph, initializes Git, creates one baseline commit, secures private files, and writes a private initialization receipt. Re-running initialization repeats complete validation before accepting the store as healthy.

The manifest also records a shipped-seed version, separate from its schema version. When a newer Jcode introduces new shipped resource paths, the composer adopts them through one isolated local commit before use. Adoption writes only missing paths introduced by that seed and the manifest update. It never overwrites an existing working file. A path committed at `HEAD` but missing from the working tree is damage and must be restored explicitly. Once a seed version is adopted, deleting one of its resources is user authority and later activations do not recreate it.

After initialization, a missing or invalid store is damage. Jcode does not silently recreate it from the shipped seed. Recovery supports restoring committed files from current `HEAD` and an explicit seed recreation. Recreation validates the replacement seed, imports, and branch before moving the damaged repository aside. A later failure reports changed state and the exact preserved backup path.

## Legacy sources

The repository service can inspect and plan exact import for current global and project:

- `.jcode/system-prompt.md`
- `.jcode/prompt-overlay.md`
- `.jcode/preferred-tools.md`
- `.jcode/swarm-prompt.md`

Import leaves the original untouched, records its SHA-256 and empty/blank semantics, validates the complete prospective repository graph, and commits the resource plus receipt together. The primary composer deactivates a global system-prompt or overlay compatibility source only after the durable receipt exists. Project compatibility fallback applies only when the corresponding managed project resource is genuinely absent. Invalid or ambiguous managed project resources remain authoritative failures, and explicit global selection is never replaced by project legacy input. An already-completed retry materializes missing committed files but never overwrites a newer working-tree edit; the typed outcome lists preserved divergent paths.

`AGENTS.md` remains a dedicated live ecosystem input. External skills remain read-only and are never imported automatically. The typed Copy backend can preserve a complete external package and commit a managed global or project copy; the central manager exposes reviewed Copy actions. See [`SKILLS.md`](SKILLS.md).

## Primary activation

The first new primary session initializes the global store, resolves any configured project repository, and passes those roots to the typed composer. Initialization, seed upgrades, and Git publication retain full-store validation. Ordinary reads of a ready, current-seed store validate only selected resources and dependencies, so an invalid unrelated roster alias or agent cannot block valid composition or notification rendering. This shared read policy covers primary selection, clear, transfer, notification occurrences, and roster loading. Repository/manifest damage and invalid selected sources still fail visibly, without seed or global-only fallback.

The global store is not initialized by ordinary server construction, read-only repository service access, internal non-primary agents, or documentation commands. See [`AGENT_PROFILES.md`](AGENT_PROFILES.md) for composition and lifecycle.

## Current boundary

The repository service, primary activation, and manager inspection/editing protocol and TUI are live. `/instructions` and `/model-roster` inspect current sources without initialization, seed upgrades, activation, or repository mutation. Mutation actions are explicit, reviewed, and preserve current session instructions. Network Git operations remain explicit and no parent-project gitlink is committed automatically.

## Notification occurrence and todo history

See [notification occurrence and todo history](NOTIFICATIONS.md) for current-source occurrence rendering, structural ownership, typed todo queues, failures and recovery.

### Committed deletion across seed upgrades

Seed adoption checks the current branch history before creating a missing shipped path. A path deliberately deleted in an earlier commit is not a new resource and is not recreated by a later seed version. A file still present at HEAD but missing from the working tree remains damage requiring explicit repair. This closes the cross-version deletion case without changing normal scoped commits or working-file authority.
