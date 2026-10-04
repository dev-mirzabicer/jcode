# Managed placement rollout

`features.managed_primary_launch` turns on workspace placement for primary
sessions. It is off by default. This guide explains what changes when it is on,
how sessions get a placement, and how to enable it on an existing installation.

Related: [workspace management](WORKSPACE_MANAGEMENT.md),
[primary input and location](PRIMARY_INPUT_LOCATION.md),
[agent workspace tool](WORKSPACE_AGENT_TOOL.md),
[runtime control](RUNTIME_CONTROL.md).

## What the flag changes

With the flag on:

- Managed launch, location changes, legacy adoption and scoped new contexts
  (Clear, Split and Transfer with a grant-carry choice) are available in
  `/workspace`, the Harness API and both SDKs.
- Every primary session needs a workspace placement before it runs. A session
  without one is *unplaced*: ordinary new sessions start unplaced, and so does
  every session created before the rollout. An unplaced session can be opened,
  read and inspected, but new input, Clear, Split and Transfer are refused
  until it is placed.
- A placed session gets the agent [`workspace` tool](WORKSPACE_AGENT_TOOL.md)
  through one tool-set change, and native file writes are limited to its write
  scope.

Isolated sub-agents are unaffected. Their location is fixed by their parent.

Catalog organization, cloning, permissions, closeout, backups and runtime
control work with the flag on or off.

## Placing a session

Placement keeps the session's working directory. The instructions and Startup
Context already captured for that directory stay correct, and nothing in the
conversation is rewritten. Placing appends one location notice that does not
start a turn.

The runtime proposes placements for the session's working directory:

- If a registered location contains it, that location is offered first,
  followed by its work area and project when it has them.
- Otherwise a new standalone location is offered at the Git worktree root, or at
  the directory itself outside Git. It belongs to no project.
- A broad root (your home directory, a filesystem root or a volume root) is
  offered but never proposed by default. Placing a session there gives it write
  access to everything below.

A different placement or working directory is available in `/workspace` →
Sessions (legacy adoption) or by launching a new session from Organization.

### TUI

Send a message in an unplaced session and the **Place this session** dialog
opens over the conversation. Your message stays in the composer and was not
sent.

| Key | Effect |
|---|---|
| `Enter` (or `y`) | Place the session as selected, then send the held message |
| `↑` `↓` | Choose another placement |
| `w` | Open `/workspace` at Sessions instead |
| `Esc` | Close; the message stays in the composer and nothing is sent |

A broad root needs a second `Enter`. If the workspace changed since the dialog
opened, it reviews the placements again once. If the connection fails while
placing, the dialog does not resend: press `r` to review again, and a placement
that did apply shows as already placed.

`/place` opens the same dialog without a held message.

### `jcode run` and `jcode repl`

Process-owned sessions have no dialog. Without `--place`, input to an unplaced
session is refused before it is recorded. With `--place`, the session takes the
proposed default placement for its directory and then runs:

```bash
jcode run --place "Summarize the open TODOs"
jcode repl --place
```

`--place` refuses a broad root such as the home directory. Run from a project
directory, or place the session in `/workspace`.

### Harness API and SDKs

`primary_control_capabilities` advertises `session_placement_version: 1`. Two
`primary_location` commands use it:

- `propose_placement { session }` returns a `proposal` with the working
  directory, catalog revision, candidates and the default index (absent for a
  broad root). It changes nothing.
- `place { request: { request, session, working_dir, expected_catalog_revision,
  placement } }` places the session as reviewed and returns the location-change
  record. A new standalone root is registered first. Retrying the same request
  returns the original record. A changed working directory or catalog revision
  is a `conflict`; review again.

Human input to an unplaced session is refused before acceptance with a message
beginning `This session has no workspace placement yet`. The Rust and
TypeScript SDKs send these commands only to a runtime that advertises the
version.

## Before the catalog exists

If the flag is on but the workspace was never initialized, input to an unplaced
session is still refused and the review reports that the workspace is not set
up. Initialize it in `/workspace` → Organization first.

## Enabling it

1. Take a catalog backup if the workspace already exists (`/workspace` →
   Backup). A backup covers organization, bindings, grants and history
   references, not checkout files or transcripts.
2. Initialize the workspace if needed and register the locations you work in
   (`/workspace` → Organization). Registering an existing checkout or directory
   does not move files or change Git branches and remotes.
3. Finish any running turns, then set the flag in `~/.jcode/config.toml`:

   ```toml
   [features]
   managed_primary_launch = true
   ```

   The runtime reads the change within a few seconds; no restart is needed.
4. Place sessions as you return to them, from the dialog or in `/workspace` →
   Sessions.

Startup Context plans and instruction-store bindings are keyed by physical
location, so they are unchanged by registering or placing.

To roll back, set the flag to `false`. Placed sessions keep their placement and
write scope, and the catalog is untouched. Unplaced sessions run without
placement again, and managed launch, location changes and adoption are
unavailable until the flag is turned back on.
