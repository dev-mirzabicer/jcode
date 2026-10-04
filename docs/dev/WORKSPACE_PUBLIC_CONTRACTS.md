# Public workspace contracts

This is the caller reference for workspace, primary-session and runtime
administration over the native protocol, the curated Harness API and both SDKs.
It is the contract later consumers (C02 interaction delivery and handoffs, C04
command center) build on. They call these operations; they never write private
catalog, Session or runtime state and never re-derive scope from paths.

## Routes and negotiation

| Contract | Native | Harness (v1.13) | Rust SDK | TypeScript SDK |
|---|---|---|---|---|
| Catalog, permissions, checkouts, operations | `workspace_probe`, `workspace` | `workspace_probe`, `workspace` (`workspace_catalog_v1`) | `workspace_capabilities`, `workspace` | `workspaceCapabilities`, `workspace` |
| Checkout closeout | `workspace` (`closeout`) | `closeout_probe`, `closeout` (`checkout_closeout_v2`) | `closeout` | `closeout` |
| Primary launch | `primary_launch_probe`, `primary_launch` | same (`primary_launch_v1`) | `launch_primary` | `launchPrimary` |
| Input, location, session inspection | `primary_control_probe`, `primary_input*`, `primary_location` | same (`primary_control_v1`) | `submit_primary_input`, `primary_location` | `submitPrimaryInput`, `primaryLocation` |
| Scoped new contexts | `workspace` (`permissions/review_carry`), `scoped_context` | `grant_carry_review`, `scoped_context` | `review_grant_carry`, `create_scoped_context` | `reviewGrantCarry`, `createScopedContext` |
| Runtime lifecycle and supervision | `runtime_probe`, `runtime_control` | same (`runtime_lifecycle_v1`, `runtime_supervision_v1`) | `runtime_control` | `runtimeControl` |

None of these creates or attaches a Session. Negotiation has two layers: the
bridge advertises a Harness capability, then the client probes the runtime's
native version. `workspace_capabilities` reports `catalog_version`,
`permissions_version`, `checkout_version`, `closeout_version`,
`management_version` and `managed_rollout`; absent fields mean the runtime
predates that contract. `WorkspaceRequest::required_capability` (TypeScript
`requiredWorkspaceCapability`) names the contract each request needs, and the
SDKs send a request only when its exact version is advertised.
`primary_control_capabilities.session_inspection_version` separately gates
`inspect_session`.

The Harness `workspace` route refuses `closeout` requests: closeout keeps its
dedicated route, version and reply envelope.

## Authority

The authenticated connection is the trusted client recorded on reviewed effects
(grant approval, closeout begin/approval, clone source trust). No request field
carries authority, and same-user trust is not physical-human attestation.
Agents use the narrower [workspace tool](../WORKSPACE_AGENT_TOOL.md), which
can propose access and prepare or conditionally complete a closeout but never
approve.

## Correlation, retry and paging

`WorkspaceRequest::matches_response` (TypeScript `matchesWorkspaceResponse`)
is the single correlation rule. A reply must describe the request's target,
request ID, proposal, Session, snapshot or reviewed intent; otherwise the SDKs
raise `UnexpectedReply` / `unexpected_reply`. A domain rejection is the typed
`error` response and always belongs to its request. Both SDKs evaluate the
committed fixture `crates/jcode-workspace-types/src/workspace_correlation.json`;
regenerate it with `JCODE_WRITE_WORKSPACE_MATRIX=1 cargo test -p
jcode-workspace-types`.

Mutations are request-idempotent: the same request ID and input returns the
original receipt; the same ID with different input is a conflict. Keep request
and review IDs across uncertain replies and inspect before retrying. Lists
return `total` and a revision-bound cursor; a cursor from an older revision is
a conflict. Page size (1 to 200) is not a membership limit.

## Consumer examples

C04 list and review (TypeScript):

```ts
const status = await client.workspace({action: "status"});
let after = null;
do {
  const page = await client.workspace({action: "list", query: {kind: "location",
    project, home: null, repository: null, visibility: "current",
    active_sessions_only: false}, after, limit: 100});
  if (page.kind === "error") throw new Error(page.value.detail);
  render(page.value.items);
  after = page.value.next;
} while (after);
const review = await client.workspace({action: "permissions", request: {
  action: "review", expected_revision: status.value.revision,
  change: {action: "issue", audience: {kind: "session", id: session},
    target: proposal.target, proposal: proposal.id}}});
// Show the review, then apply it with a caller-retained request ID.
```

C02 launch and input (Rust):

```rust
let record = client.launch_primary(PrimaryLaunchRequest { request, expected_revision, input })?;
let receipt = client.submit_primary_input(PrimaryInputEnvelope { id: input_id,
    session: record.session.clone(), delivery: PrimaryInputDelivery::SafeBoundary, ..envelope })?;
let view = client.primary_location(PrimaryLocationCommand::InspectSession {
    session: record.session })?;
```

`scripts/verify_workspace_contracts.py` runs both journeys against an isolated
daemon and the curated bridge.

## Versions

Harness API 1.13 adds `workspace_probe`/`workspace`, the
`workspace_capabilities`/`workspace` events and
`primary_control_capabilities.session_inspection_version`. Older clients ignore
the new events; older runtimes are refused before a request is sent.
