# Instruction stores

**Status:** Phase 3 repository infrastructure. Production prompt activation and the central manager arrive in later Phase 3 packages.

Jcode has a typed service for Git-versioned managed instructions. The service is present in the server, but it does not initialize or migrate Mirza's live instruction store merely because Jcode starts.

## Locations

### Global

```text
~/.jcode/instructions/
```

The global store is an owner-only standalone Git repository. It uses local branch `main` by default. The rest of `~/.jcode` is not part of this repository.

### Project submodule

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

It is schema-versioned. Modes are `submodule`, `external-remote`, `external-local`, and `standalone`. Invalid explicit configuration fails visibly. Jcode does not silently ignore it and continue as global-only.

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

Affected resource and dependency behavior is compared with the expected committed `HEAD`. New resource, reference, or dependency errors block the commit and leave the working edit visible for repair, including on retry. Existing unrelated resource errors do not block a valid edit, and multi-path operations can repair references atomically. Unrelated staged, dirty, and untracked repository state is preserved. Detached repositories cannot Save until a branch is selected. Restore writes historical content as a new commit. No-op Save and restore create no commit.

Submodule, external-checkout, and standalone setup also carry operation identities. A retry reuses the repository already created by an interrupted setup while the same repository lease prevents concurrent setup from racing it.

Ordinary operations do not offer reset, rebase, force push, commit deletion, or history rewrite.

## Explicit synchronization

Fetch, pull, push, branch checkout, branch creation, remote configuration, and repository setup are explicit operations. Jcode never automatically pulls or pushes.

Pull requires a clean instruction repository. Fast-forward-only and merge behavior are distinct. Conflicts remain visible and preserve local work.

## Initialization and recovery

First installation materializes a complete seed, plans eligible exact legacy imports, validates resources, initializes Git, creates one baseline commit, secures private files, and writes a private initialization receipt.

After initialization, a missing or invalid store is damage. Jcode does not silently recreate it from the shipped seed. Recovery supports restoring committed files from current `HEAD` and an explicit seed recreation that first moves the damaged repository aside.

## Legacy sources

The repository service can inspect and plan exact import for current global and project:

- `.jcode/system-prompt.md`
- `.jcode/prompt-overlay.md`
- `.jcode/preferred-tools.md`
- `.jcode/swarm-prompt.md`

Import leaves the original untouched, records its SHA-256 and empty/blank semantics, validates the managed resource, and commits the resource plus receipt together. Runtime deactivation happens only after a later activation package adopts the durable receipt.

`AGENTS.md` remains a dedicated live ecosystem input. External skills remain read-only until an explicit Copy workflow is implemented. Neither is imported automatically.

## Current activation boundary

Phase 3 WP-02 does not:

- Initialize Mirza's live store
- Change the current system prompt
- Disable a legacy prompt source
- Add agent commands
- Change skill activation
- Add the central instruction manager UI

Repository behavior is currently exercised through typed Rust interfaces and sandboxed real-Git fixtures. Phase 3 WP-03 adopts the roots for frozen system composition. WP-09 and WP-10 expose inspection and mutation through the central manager.
