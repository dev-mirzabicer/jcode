# Workspace agent tool

Placed primary sessions can inspect their workspace location, request write
access to another location and work on a checkout closeout that a human
started. They use one `workspace` tool. Every grant, approval and closeout
authorization still comes from a trusted client, normally the human
[`/workspace` management mode](WORKSPACE_MANAGEMENT.md).

## Who sees the tool

The tool is advertised only to a primary Session that has a workspace
placement (project, work area, checkout, directory or standalone location).
Legacy (unplaced) sessions and isolated sub-agents never receive it. A legacy
session that is later adopted receives it once through an appended tool-set
notice. Children cannot call it even through a cached definition: `workspace`
is in the child policy's administrative tool list.

Placed sessions arise from managed launch, placement of an unplaced session
(the TUI placement review, `/place`, `jcode run --place` or `/workspace`
adoption) and scoped new contexts, all behind
`features.managed_primary_launch`. With that flag off, no new session is offered
the tool. See [managed placement rollout](WORKSPACE_ROLLOUT.md).

## Actions

| Action | Effect |
|---|---|
| `inspect` | Placement, home chain (project, work area, repository), placement root, cwd, scope counts, immediate members and pending proposals |
| `list` | Paged members of a project, work area or repository (`visibility` current/all/archived/retired/closed) |
| `entity` | One catalog record |
| `locate` | The deepest registered root that owns a path, and whether this Session may write there now |
| `scope` | Paged writable roots with their permission sources (ordinary placement or grants). Only the returned roots are physically verified |
| `request_access` | A durable pending proposal for a location root, or the member roots of a project or work area |
| `access_status` | This Session's proposals, or one of them |
| `closeouts`, `closeout`, `closeout_inventory` | A location's closeout operations, one closeout with its removal review, and its inventory pages |
| `closeout_action` | One closeout step (below) |
| `closeout_action_status` | A step's recorded result by request ID |

Lists page with `limit` (1 to 200, default 50) and an opaque `after`
continuation. The page size is not a membership cap: `total` is complete and a
continuation from an older catalog revision is a conflict, not a silent gap.

## Authority

The tool reads the caller's authoritative Session from storage. Model
arguments select targets only. Fields such as `session`, `approved` or
`grant` are ignored; a proposal always binds the calling Session and starts
pending. There is no approve, decline, grant or revoke action. A proposal
grants nothing until a trusted client approves it; enforcement then follows
the grant's durable revision.

Each tool invocation derives one stable domain request ID, so a recovered or
replayed invocation converges on the original receipt.

## Checkout closeout steps

A human begins every closeout, optionally with the **Don't ask for approval if
there are no ambiguities** checkbox (conditional authorization). The agent's
steps need the checkout to be in the Session's current write scope:

- `refresh`, `preserve`, `disposition` (recorded with the Session as its
  author) and `review_removal`.
- `declare_no_loss` with the review ID and the agent's assessment. It succeeds
  only under the human-issued conditional authorization, for the exact current
  review, with complete preservation, no unresolved findings and no live work.
  The assessment cannot waive a structural finding. Trusted clients cannot
  issue this declaration.
- `finish`, only for a removal this Session declared. A human-approved removal
  is finished by the human client.

Removal approval and recovery decisions are not available to agents. Each step
runs under the same retained execution supervisor as human closeout actions;
the tool waits for the step to settle and returns its record. If the wait is
cancelled, the step continues under its owner and `closeout_action_status`
reads it.

## Guidance and cache behavior

The approved framework-stage operating text is the managed tool-guidance
resource `tools/workspace.md` (instruction seed 31), appended to the tool
description. It is captured on the Session after the first successful request
preflight and reused unchanged across reload, restart, resume and split, like
the delegation guidance. When the provider receives the tool inside a tool-set
notice (Claude), the captured text is the one that notice carried. Final prose
belongs to SP-58-C05.

The tool's arrival is one recorded tool-set change. Later placement or grant
changes append location and write-access notices; they never rebuild the tool
definition.

A placed session's initial Session Context block also carries compact location
facts: placement, home chain, placement root, writable-root counts, the first
members of a project or work area, and pending proposals. A session placed
before its first request (the [placement review](WORKSPACE_ROLLOUT.md)) receives
the same facts in its one location notice message. Unplaced sessions are
unchanged.

## Limits

Native enforcement covers Jcode's structured file tools only. Shell commands,
external CLIs, MCP and browser effects follow the same scope by instruction and
are not contained by it. Same-user trusted clients are not physical-human
attestation. Native closeout requires the supported macOS adapters; elsewhere
closeout steps report `UnsupportedCapability`.

Related: [public workspace contracts](dev/WORKSPACE_PUBLIC_CONTRACTS.md),
[new-context scope](PRIMARY_CONTEXT_SCOPE.md),
[native write scope](dev/NATIVE_WRITE_SCOPE.md),
[checkout closeout](dev/WORKSPACE_CLOSEOUT.md).
