# Workspace management

`/workspace` opens Jcode's management mode for projects, checkouts, session
locations, permissions, checkout closeout, catalog backups and the runtime.
`/runtime` opens the same mode at its Runtime section. It is a focused,
functional client over the existing workspace, primary and runtime owners. It
is not the final command center, and the session picker is unchanged.

The Niri-style session rows that used `/workspace` before are now `/niri`
(`/niri status|on|off|add [right|up|down]`).

## Where it runs

The mode talks to the shared runtime through the attached client connection.
A client started in private local mode explains that management needs the
shared runtime and sends nothing. Opening the mode does not create a session,
initialize the catalog, or change any state.

Each section reads paged, metadata-only lists from its owner and keeps the
selected item by its stable ID across refreshes, resizes and reconnects. Capability
probes run first. A server that lacks a capability is reported as such, and the
corresponding actions stay unavailable rather than failing after the fact.

## Navigation

| Keys | Effect |
|---|---|
| `1`–`7`, Tab, Shift-Tab, or a click on a tab | Organization, Sessions, Operations, Permissions, Closeout, Backup, Runtime |
| Up/Down, `j`/`k`, PgUp/PgDn, mouse wheel, click | Select a row |
| Enter | Open details in narrow layouts |
| `[` / `]` | Previous / next page |
| `r` | Refresh |
| `?` | Every action for the current section, with its key |
| Esc, `q` | Back, then close |

The footer shows the section's actions and always keeps Help and Close visible.
Footer entries are clickable. Mouse capture follows the terminal's usual Shift
gesture for text selection.

Layouts: list and details side by side from 120 columns, a single pane with
details on Enter at 80 and 60 columns, and a minimum of 48×12. Below that the
mode says so and still accepts Esc. Colors follow the Jcode palette and degrade
to text and underline cues under `NO_COLOR`.

## Forms, reviews and uncertain outcomes

Actions that change state open a form, then a review, then send the reviewed
request. Nothing is sent while you edit.

- Forms: Up/Down or Tab move between fields, Left/Right or Space change a
  choice, Ctrl+U clears a text field, Ctrl+S submits. Esc on an edited form asks
  for a second Esc before discarding it.
- Reviews show what the owner will do, including the exact target and revision.
  `y` confirms, `n` or Esc declines. Enter presses the highlighted button, which
  is Cancel by default. Escalations (closeout approval and removal, catalog
  restore, runtime Force) require typing a word.
- Your draft survives a rejected or declined review. If the catalog changed since
  the draft was opened, the review is refused, the catalog status refreshes, and
  the draft returns with the reason. Ctrl+S reviews it against current state.
- Drafts, selections and target IDs persist through resize and reconnect.
- A reply that is lost after an effect was sent is listed under **Unknown
  outcomes** (`U`). Inspect the current state first. `R` retries the *same*
  request, which its owner deduplicates. `D` dismisses the entry. Nothing is
  retried automatically.

The mode is a trusted same-user client. It is not proof that a person pressed a
key, and native write policy is not a shell sandbox.

## Organization (`1`)

Lists projects, logical repositories, work areas and locations (checkouts,
directory references and standalone roots). `f` filters by kind and `h` cycles
visibility: current, all, archived, retired and closed.

- **Initialize** (`I`): only offered when the catalog was never created. Damaged or
  incomplete catalogs are reported as such and are not offered a fresh start.
- **Create** a project (`n`), repository (`N`), work area (`a`); **associate** a
  repository with a project (`A`); **register** an existing path (`g`) as a
  checkout, directory or standalone root. Registration never moves files.
- **Edit**: rename (`e`), move or adopt into a project/work area (`m`), rebind after
  an external relocation (`b`), archive/unarchive (`z`), retire (`x`), discard an
  unused record (`D`), and set a volume's default checkout directory (`v`).
- **Clone** (`c` on a repository): choose home, remote or local source, base,
  branch mode, remotes, mounted volume, default or custom destination,
  submodules and LFS. The review shows the resolved commit and destination. The
  clone runs under the runtime; follow it in Operations.
- **Startup Context copy** (`p` on a checkout): copies the ordered selection, not
  file bodies, after a review of both plans.
- **Launch** (`l`): choose placement (including a new projectless standalone root), an existing cwd or a new empty directory (which becomes the root of a new standalone location),
  and optional agent, model and effort. A checkout proposes its root. Launch never
  clones.
- **Close out** (`o` on a checkout): see Closeout below.

## Sessions (`2`)

Shows this client's session and the sessions placed at the selected catalog
entity. `i` inspects any session by ID. Details show placement, current and
initial cwd, location revision, pending moves and write scope.

- **Move / change cwd** (`m`): moving into a checkout proposes its root. Moving to
  a project or work area keeps the current cwd by default. The change applies at
  the session's next safe boundary; pending changes can be cancelled (`x`).
- **Adopt legacy** (`a`): a session with no recorded placement is reviewed against
  its recorded working directory before it receives one.
- **New context** (`n`): Clear, Split or Transfer. When the session has direct
  grants, a grant-carry review asks whether to carry them as new, independently
  revocable grants.
- **Open here** (`o`) or **in a new terminal** (`t`), **write scope** (`w`),
  **grant access** (`g`), and scope **reconcile** (`R`) / **abandon** (`X`) for
  interrupted new-context publication.

## Operations (`3`)

Durable clone, closeout, Startup Context copy, launch and location operations,
newest first. `u` toggles finished items. Clones can be cancelled (`C`), resumed
(`R`), have their submodule/LFS sources reviewed before contact (`T`), and show
their retained output (`O`). Operations are found without knowing their request
IDs, including after a client restart.

## Permissions (`4`)

`v` switches between access proposals, grants and imported grants.

- **Approve** (`a`) a pending proposal: the form is prefilled with its exact
  session and target, and the review lists the roots that become writable.
  **Decline** with `d`.
- **New grant** (`n`) for a session, project, work area or checkout audience.
- **Revoke** (`x`) takes effect at its durable revision.
- Imported grants stay disabled until bound to verified local identities (`b`) and
  activated (`A`).

## Closeout (`5`)

Begin from a checkout with `o`. The **Don't ask for approval if there are no
ambiguities** checkbox is off by default. It records a conditional authorization
for this checkout and this closeout only; the agent route that uses it is not yet
exposed.

Then: refresh the inventory (`f`), record dispositions (`d` from the inventory
pane `i`, which pages with `[` and `]` for large checkouts), preserve (`p`), review removal (`w`), approve with the typed word
(`a`), and finish removal with the typed word (`F`). Live work, stale reviews,
changed paths and unpreserved data block removal. Revoke an unfinished closeout
with `X`. Recovery reviews (`y`/`Y`), removal progress (`g`) and checkout history
(`H`) remain available, including after the files are gone.

## Backup (`6`)

Back up the catalog now (`b`), export a project definition (`e`), review and apply
an import (`i`) with explicit collision handling, and restore a snapshot (`R`,
typed). Backups and exports contain catalog metadata only. Imported grants stay
disabled until reviewed.

## Runtime (`7`, or `/runtime`)

Shows the runtime identity, desired state, current shutdown operation, power and
supervision state, the macOS login service and interrupted turns.

- **Stop** (`s`) or **Restart** (`R`): choose finish-current or interrupt, and
  whether supported native tasks keep running. The review lists affected work.
  While waiting, **Cancel** (`c`), **change options** (`h`), **retry quiescence**
  (`y`) or **Force** (`F`, typed) are available as the operation allows.
- **Interrupted turns** after an unexpected exit: **Continue** (`C`) or **Leave
  stopped** (`L`). Each decision is bound to the item's revision and sent once.
- **Start** (`S`): when the runtime is stopped, the section shows its durable
  intent read from local state (it starts nothing by itself). `S` runs the
  explicit `jcode runtime start` for this socket, then the client reconnects.
  An intentional Stop is never undone automatically.

`jcode runtime …` remains the scripted and offline control. See
[runtime control](RUNTIME_CONTROL.md).

## Staged features

`features.managed_primary_launch` still gates managed launch, location changes,
legacy adoption and scoped new contexts. With it off, these actions say that they
are staged. Catalog organization, cloning, permissions, closeout, backups and
runtime control do not depend on it. Agent-facing workspace tools are not exposed.

Related: [primary input and location](PRIMARY_INPUT_LOCATION.md),
[new-context scope](PRIMARY_CONTEXT_SCOPE.md),
[catalog](dev/WORKSPACE_CATALOG.md), [checkouts](dev/WORKSPACE_CHECKOUTS.md),
[closeout](dev/WORKSPACE_CLOSEOUT.md), [native write scope](dev/NATIVE_WRITE_SCOPE.md).
