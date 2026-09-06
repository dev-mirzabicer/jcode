# Instruction stores

**Status:** Phase 3 Git repository infrastructure, primary activation, versioned seed adoption, and the [read-only instruction manager](INSTRUCTION_MANAGER.md). Mutation UI remains WP-10 work.

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
- An intentionally empty file remains valid.
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

Completed-operation recovery matches the exact operation trailer, not an ID
prefix or incidental commit-subject text. Recognizing a published Save does not
refresh the ordinary index again: the user may have staged newer target content
since that Save. If interruption left an index/worktree difference, inspection
shows it rather than silently resetting it during a retry.

## Explicit synchronization

Fetch, pull, push, branch checkout, branch creation, remote configuration, and repository setup are explicit operations. Jcode never automatically pulls or pushes.

Pull requires a clean instruction repository. Fast-forward-only and merge behavior are distinct. Conflicts remain visible and preserve local work.

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

`AGENTS.md` remains a dedicated live ecosystem input. External skills remain read-only and are never imported automatically. The typed Copy backend can preserve a complete external package and commit a managed global or project copy; the central manager action arrives in WP-10. See [`SKILLS.md`](SKILLS.md).

## Primary activation

The first new primary session initializes the global store, resolves any configured project repository, and passes those roots to the typed composer. Initialization, seed upgrades, and Git publication retain full-store validation. Ordinary reads of a ready, current-seed store validate only selected resources and dependencies, so an invalid unrelated roster alias or agent cannot block valid composition or notification rendering. This shared read policy covers primary selection, clear, transfer, notification occurrences, and roster loading. Repository/manifest damage and invalid selected sources still fail visibly, without seed or global-only fallback.

The global store is not initialized by ordinary server construction, read-only repository service access, internal non-primary agents, or documentation commands. See [`AGENT_PROFILES.md`](AGENT_PROFILES.md) for composition and lifecycle.

## Current boundary

The repository service, primary activation, and read-only manager protocol/TUI are live. `/instructions` and `/model-roster` inspect current sources without initialization, seed upgrades, activation, or repository mutation. Editing workflows remain WP-10 work. Network Git operations remain explicit and no parent-project gitlink is committed automatically.

## Notification occurrence and todo history

See [notification occurrence and todo history](NOTIFICATIONS.md) for current-source occurrence rendering, structural ownership, typed todo queues, failures and recovery.

### Committed deletion across seed upgrades

Seed adoption checks the current branch history before creating a missing shipped path. A path deliberately deleted in an earlier commit is not a new resource and is not recreated by a later seed version. A file still present at HEAD but missing from the working tree remains damage requiring explicit repair. This closes the cross-version deletion case without changing normal scoped commits or working-file authority.
