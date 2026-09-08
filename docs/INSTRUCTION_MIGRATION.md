# Migrating and recovering instruction sources

Start with the [instruction overview](INSTRUCTIONS.md). This guide covers source
cutover and old sessions, not changes to prompt wording or agent behavior.

## Existing installations

The global store is a standalone private Git repository at
`~/.jcode/instructions`. First primary activation creates it when it has never
been initialized. The rest of `~/.jcode` is not versioned with instructions.
Ordinary server construction and manager inspection do not initialize it.

`instruction-store.toml` records `schema_version`, `seed_version`, optional
`default_agent`, and durable legacy-import receipts. These are store metadata,
not prompt-source versions attached to running sessions. New seed paths are
adopted through a scoped local commit. Existing working files and historical
committed deletions are preserved. Seed adoption never pushes a remote.

An initialized store that is missing or damaged is not a fresh installation.
Use the manager's recovery actions or restore a known backup. Explicit seed
recreation first validates its replacement and preserves the old repository at
a named backup path. Do not delete initialization records to force bootstrap.

## Legacy file cutover

| Existing source | Managed destination | Activation after cutover |
|---|---|---|
| `.jcode/system-prompt.md` | `agents/jcode.md` | Managed compatibility profile |
| `.jcode/prompt-overlay.md` | `system/common.md` | Paired common-guidance slot |
| `.jcode/preferred-tools.md` | `tools/preferred-tools.md` | Paired tool-guidance slot |
| `.jcode/swarm-prompt.md` | `tools/swarm-routing.md` | Captured Swarm tool-description guidance |
| Global/project `AGENTS.md` | Not imported automatically | Dedicated ecosystem input at system activation |
| External/plugin skill package | Only explicit Copy creates a managed package | Existing external source remains discoverable until shadowed |

Global system/overlay imports occur at initial store creation. Preferred-tool
and Swarm-routing cutovers also support their introduced seed-upgrade boundary.
An old global file added after its cutover is not an automatic override. Use the
manager's managed definition or explicit legacy-import workflow instead.

Projects without a managed store retain compatible project files. Project setup
is explicit. Once configured, use **Imports and original compatibility files**
to prepare and review an import. Saving commits the captured managed source and
its cutover receipt together. Original files remain untouched on disk. A present
managed definition plus an unimported competing legacy definition is an explicit
conflict, not two contributions or permission to overwrite silently.

The import preserves original source bytes in the managed resource. Existing
legacy runtime trimming and provenance headings remain where the compatibility
contract requires them. This is why raw file equality and rendered prompt
equality are different evidence. Managed plain text does not accidentally
interpret literal `{{ ... }}` examples. New template interpretation is opt-in.

After import, editing the original legacy file does not alter active managed
instructions. A missing imported target blocks its affected use rather than
reviving the original. To suppress prose deliberately, clear the managed **body**
while retaining valid metadata, if the consumer permits empty content. Deleting
a project redefinition reveals global source. A blank TOML manifest or roster,
missing required metadata, and a missing registered resource are not intentional
empty bodies.

## Project configuration

Prefer the manager's reviewed **Project setup** workflow. It binds paths to the
server's project and reports parent Git consequences. Explicit configuration is
`<project>/.jcode/instructions.toml`; an invalid configuration fails visibly.
A conventional true submodule can be discovered without an explicit file.

For an existing independent local instruction checkout, the serialized shape is:

```toml
schema_version = 1

[repository]
mode = "external-local"
path = "/absolute/path/to/instruction-checkout"
branch = "main"
```

Other modes are `external-remote` with `url` and `branch`, `submodule` with `path`
and optional `url`/`branch`, and `standalone` with `path` for a non-Git project.
Editing configuration does not clone or repair an unavailable checkout. Use the
explicit setup/recovery action to prepare it. Do not point an instruction store
at an enclosing ordinary source repository.

A Save commits only the instruction repository. For a submodule, the parent
project's changed gitlink remains uncommitted for its owner to record. Renaming
or deleting a resource repairs references only within its owning repository.
Known active-project cross-repository dependencies block removal and identify the
explicit migration needed. Jcode does not claim to scan every unopened project.

## Old saved sessions

A session already carrying frozen system/skill state resumes from those exact
bytes without source access. Split clones that state. Editing disk files, config
reload or skill-registry reload does not revise it.

A legacy session without frozen system state cannot recover an exact historical
prompt that was never stored. Restore composes current explicit `global:jcode`,
persists it on the candidate session before adopting it live, and clears old
provider-native continuation. It does not rewrite authoritative messages or
claim historical prompt reconstruction. Failed migration leaves the current
live session unchanged.

Ordinary post-dispatch `/agent` appends a complete profile. To install current
source as the true system prompt, use explicit idle `/agent replace <selector>`.
Clear and transfer start new contexts and render current source for the retained
agent. See [profiles](AGENT_PROFILES.md) for their cache and context consequences.

## Recover without losing newer work

| Symptom | Recovery |
|---|---|
| Source preview differs from active instructions | Expected after source edits. Inspect the stored session and choose explicit replacement only if intended. |
| Present invalid project source | Repair that source or explicitly select global where appropriate. Invalid specificity does not silently fall through. |
| Missing committed resource | Prepare Restore committed version or select a historical revision, review and Save. |
| Detached repository | Select or create an attached branch before Save. A live attached draft blocks branch changes. |
| Stale draft | Compare opened/current/proposed text, explicitly choose a new base, then review and Save. Original draft remains recoverable. |
| Interrupted or lost Save reply | Recover the structural operation receipt. Do not assume rollback or submit a guessed new Save. |
| Unsent client form/editor values | Recover local unsent changes. They are separate from server drafts and never replay source/network actions automatically. |
| Interrupted network action | Inspect the retained outcome and actual remote state before deciding whether to repeat it. |
| Diverged branch or unfinished Git transaction | Finish or abort it deliberately with Git. Manager pull is fast-forward-only and does not rewrite history. |
| Provider budget block | Instructions remain complete. Use existing context controls or a suitable route; do not shorten source silently. |

Interactive callers retain actionable errors and recovery paths. CLI and Harness
creation fail before model dispatch and clean unpublished session state.
`WorkingTreeOnly` is ordinary runtime policy. Future unattended callers may opt
into `AllowHeadFallback` only for genuinely absent committed working files.
Present invalid, unreadable, empty-invalid or symlinked content never triggers
that fallback. Owning future callers still decide permissions, retries and
unattended failure policy.

Managed repositories and draft state are private, but instruction text is not a
secret-storage facility. Explicit instruction/session exports include complete
current instructions. Startup Context retains its separate receipt-only default
export policy. Git credentials stay with Git authentication tooling, not prompt
files, draft content or model-roster descriptions.
