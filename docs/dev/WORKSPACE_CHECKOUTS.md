# Independent checkout provisioning and adoption

This is the current **service contract**, not the final workspace-management
TUI. An authenticated same-user client can review and execute operations through
`workspace` on the shared runtime without a provisional inference Session.
`workspace_capabilities.checkout_version=1` negotiates this surface. Ordinary
managed primary launch remains gated (`managed_rollout=false`), and no agent
workspace tool, checkout deletion, hosted fork, integration target, or final
closeout skill is exposed by this service. The human management mode is a
separate consumer. The client must not claim that this backend is a completed
human-facing clone form.

## Deliberate clone journey

1. Create or choose a project/work area and an explicitly associated logical
   repository through the existing [catalog](WORKSPACE_CATALOG.md). `volumes`
   returns mounted UUID, label, mount, writable and free-space observations.
   Select one volume and its default or an absolute custom destination.
2. `review_clone` names the home, repository, display name, local checkout or
   remote source, exact branch/tag/advertised commit base, keep/create/detached
   branch behavior, resulting remote names/URLs and explicit submodule/LFS
   choices. A local source's dirty/untracked content is never requested for
   copying. A remote clone suggests its source as `origin`; local acquisition
   does not make the local directory a push destination. The review binds a
   source commit, volume, destination and current catalog revision.
3. `begin_clone` takes the review and a caller-generated request UUID. It
   returns an operation/Location identity before completion. `inspect_clone`
   observes its stage, progress, retained-output run IDs and any issue. A lost
   reply is resolved with the same request, not a second checkout.
4. The runtime owns acquisition independently of the client connection. It
   records the operation before effects, pins a witnessed destination parent,
   creates an owned same-volume `.jcode-clone-<operation>` stage and runs
   ordinary Git with `--no-local`, `--no-hardlinks`, full object history and
   no alternates for the top-level repository. Git's submodule helper can
   still hardlink local source objects, so the service copies and fsyncs
   any borrowed object inode inside the owned stage before publishing.
   It checks out the reviewed commit, selects the reviewed
   branch, materializes and verifies requested submodules at recorded gitlinks
   and LFS bytes, and installs only reviewed resulting remotes. Readiness
   requires an exact HEAD, branch/remotes, clean tree, recursive submodules,
   content hashes, fsck, stage witness and reviewed volume.
5. A marker in the independent clone's `.git` directory identifies its exact
   operation. Publication uses exclusive no-replacement rename on the same
   volume, then the catalog binds one Ready `ManagedClone` location. A restart
   between rename, catalog commit and reply reconciles that same witnessed
   stage. A conflicting final path is never adopted as a completed clone.

`cancel_clone` durably requests cancellation. It stops owned Git process work,
not already completed filesystem effects. Only an empty *witnessed owned*
stage may be removed automatically. Nonempty, changed or ambiguous stages
remain available for inspection. `resume_clone` reuses the exact operation
only after the prior execution owner proves its run terminal. An unknown live
owner blocks reacquisition; a failed/recovery-required stage is never
automatically reset or deleted. A failed automatic catalog backup is reported
on an already Ready checkout and can be retried without repeating Git work.
If the retained execution output itself cannot be sealed after physical
publication, the Ready checkout retains a separate `output_issue`; failure to
record diagnostics is not misreported as filesystem rollback.
No operation moves/removes another user's existing path to create a clone.

The selected source's standard credential helper and SSH agent retain
credential custody. Embedded URL credentials, ambient Git path redirection,
unrequested hooks/templates/filters, shell command injection, unknown Git
options and interactive authentication are rejected/neutralized. A selected
local/relative submodule URL and an explicit `.lfsconfig` endpoint require
separate review. Git LFS is required when requested and missing dependency
errors are actionable. Only the new clone receives repository-local LFS
filters (`--skip-repo` avoids installing hooks). No global Git configuration,
project setup command, package-manager install or background service is run
as a consequence of cloning. Auth and network failure leave an inspectable
incomplete operation, not a Ready checkout.

## Adopt and repair an existing root

Existing Git repositories, including linked worktrees, remain existing data.
`review`/`apply` with `RegisterLocation` binds a physical root to a repository
and single project/area home without changing files, refs or remotes. The
catalog classifies a real linked worktree by its per-worktree metadata, not
merely by the presence of a `.git` file. `AdoptStandalone` explicitly gives a
projectless existing location a home and retains its stable location ID.
Non-Git directories remain references, not clone/copy targets. Bare and
damaged Git roots fail instead of becoming session checkouts.

`RebindLocation` reviews the existing location ID, exact old path and binding
generation, and a new verified physical root. Its atomic binding update
increments the generation and records an inspectable `location_rebind`
operation. It does not move files, silently convert an independent clone into
a linked worktree, reactivate a closed root, or update Session cwd/history.
If the original root still exists as a different physical root, the review
refuses an implicit replacement. An offline/replaced root stays inspectable
while a new binding is explicitly reviewed. A session with an old cwd still
needs its separately reviewed location/cwd control before tool-enabled work.

Startup Context and instruction stores remain keyed to *concrete physical*
identity. Rebinding a catalog Location does not rewrite older captured
messages, saved plan keys, or instruction-store configuration. When desired,
`review_startup_copy` and `apply_startup_copy` explicitly copy the **ordered
path-only selection** to a Ready bound target. Review names the source root,
target Location/generation, source/target plan revisions and each resolved
target. Every external target needs fresh resolved-path approval, even if the
source plan already approved a file. The plan owner's existing selector,
editor lease and revisioned transition own validation and saving. A journaled
copy request is idempotent across a lost acknowledgement or a crash between
plan Save and catalog receipt. `inspect_startup_copy` distinguishes pending,
complete and recovery-required states. File bodies are never copied into the
target default: a future session captures its *current target files* through
the normal Startup Context owner. The copy is a separate outcome from the
clone, so failure cannot undo or falsely complete a Ready checkout.

## Safety and evidence boundaries

The catalog and root leases coordinate cooperating Jcode operations. External
same-user writers and arbitrary shell effects are not an OS sandbox. Volume
identity is a checked UUID, not a label; a missing volume never becomes a
fallback directory. Free space is an observation, not a reservation. An I/O
or ENOSPC failure retains the known operation/stage and reports partial truth.
Credentialed hosted Git servers and native Linux/Windows volume behavior
require their own acceptance evidence. The native acceptance route uses
isolated disposable Git repositories and an owned loopback LFS transfer
service, not real checkout removal or real volume disruption.

Implementation owners: `jcode-base::workspace::{checkout,organization,startup_copy}`
and `jcode-base::location::volume` own Git/catalog/physical facts;
`jcode-app-core::workspace::{clone_runner,startup_copy}` owns durable runtime
execution and existing Startup Context editor coordination;
`jcode-workspace-types` and `jcode-protocol` carry typed operations. The
execution store owns retained Git output and actual child process control.
Reproduction uses focused `workspace::checkout`, `workspace::startup_copy`
and protocol tests, plus `scripts/test_workspace_checkouts.py` through
`scripts/run_isolated_test.py` against an activated TUI binary.
