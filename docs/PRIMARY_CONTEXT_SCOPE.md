# Reviewed scope for new primary contexts

This backend extends the existing Clear, Split and Transfer owners. The
[workspace management mode](WORKSPACE_MANAGEMENT.md) offers the reviewed carry
choice (Sessions, `n`). No agent administration tool is advertised. Scoped new contexts follow the
rollout flag ([managed placement rollout](WORKSPACE_ROLLOUT.md)); an unplaced
session must be placed before Clear, Split or Transfer. Native enforcement is described
in [native write scope](dev/NATIVE_WRITE_SCOPE.md).

## Review and choice

A source with active, same-installation direct Session grants requires one explicit
carry or do-not-carry choice before a replacement context is created. Missing choice
returns `NeedsGrantChoice` before Session allocation or transfer summarization.
An ordinary new-context operation with no direct grants needs no permission question.
Disabled/imported definitions and other audiences are not direct Session grants.

The trusted catalog operation `permissions/review_carry` reads authoritative Session
placement and current direct grants. Its review binds the source, location and
catalog revisions, not a client's selected row or supplied path. The review itself
confers no permission. A choice names that review and a boolean `carry`.

Carry creates fresh grant IDs addressed to the destination Session, with
`copied_from` provenance and independently owned revocation. Revoking an original
or a copy does not revoke the other. Inherited project/area/checkout audience
access is recomputed from current organization, not copied into direct grants.
Earlier grant notices or conversation ancestry cannot authorize a new Session.

The receiving cwd remains explicit. Current inherited grants and reviewed direct
copies can authorize it without changing organizational placement. Dropping the
only grant that covers a retained cwd blocks preparation instead of silently
moving the Session or widening its scope. Choose an ordinary cwd or carry the
applicable access through the reviewed path.

## Existing context semantics

- Split preserves the existing continuation transcript, exact frozen system/skill,
  Startup Context and reversible projection. It appends the established fork notice.
- Clear starts an independent primary context, refreshes instructions and Startup
  Context, clears the active skill and does not inherit conversation ancestry.
- Transfer preserves its existing summary/todo semantics, selected profile,
  concrete route/effort and cwd while refreshing instructions and Startup Context.
- Shared Clear leaves the source and peer attachments intact. Only its requesting
  client switches to the replacement. Split and Transfer return a destination to
  attach explicitly.
- Local and hosted split/transfer preparation share the primary owner. Local TUI
  Clear prepares the complete replacement before changing its selected context.
  Scope failures preserve the old context and input.

This work does not grant a continuation control over the source's isolated children.
Their immutable original-parent relationship remains separate.

## Publication and recovery

`Session.scope_copy` records an operation ID and a readiness bit. Creation first
records catalog intent and installs an unready marker. The existing creator then
prepares all context and auxiliary state. It seals readiness in the same final
Session checkpoint as that prepared history. An intermediate constructor snapshot
cannot activate copied grants.

The catalog subsequently commits fresh copies, the derived Session index and its
receipt in one transaction. The Session remains authoritative for context and
placement. The catalog owns permission and publication receipts. No SQL transaction
spans provider inference, and ordinary grant revocation is not postponed by a long
context preparation. Source changes invalidate stale uncommitted reviews.

A review can bind only one destination, including racing attempts. Reconciliation
of a committed receipt never repeats grant issuance or resurrects a revoked copy.
Ready interrupted checkpoints reconcile through normal publication restoration or
an explicit trusted recovery operation. Unready, corrupt, stale or restore-invalidated
intent fails closed. There is no automatic repeated transfer inference.

The existing workspace protocol exposes `context_scope_status` by review and
`reconcile_context_scope` by retained Session. `abandon_context_scope` marks only
unpublished intent failed, retaining history. It refuses to undo published copies.
Unpublished-constructor cleanup uses that same boundary before removing its own
artifacts. Failed reconciliation identifies the retained Session in its error.
Automatic catalog backup completion is separately recoverable from the committed
permission outcome.

## Public clients

`primary_control_probe` advertises optional `context_scope_version=1`, separately
from legacy adoption and ordinary input. Both SDKs negotiate it. Creation additionally
requires managed controls to be enabled. Review can inspect supported staged state.

- Rust: `review_grant_carry`, then `create_scoped_context` with a `NewContextKind`
  and `GrantCarryChoice`.
- TypeScript: `reviewGrantCarry`, then `createScopedContext` with the same typed
  intent.

The source must be attached for context creation. Clear updates that attachment
before its correlated created reply. The reply names source, destination and kind.
Clients reject foreign or mismatched replies. Scope rejections preserve their typed
workspace issue through the existing primary-creation SDK error category. Transport
Ack is not publication. After an uncertain result, inspect the original review's
retained scope status instead of allocating a new review and repeating effects.

The native administrative recovery operations remain the workspace service
contract. The management mode consumes them (Sessions, `R` and `X`) without
rewriting Session files or sequencing its own permission transaction.

## Verification boundaries

Mechanism checks use real private catalog/Session stores, captured context and
native mutation paths. They cover missing/yes/no choices, stale and reused reviews,
ready checkpoints, interrupted commit/replay, independent revocation, inherited
scope, grant-aware cwd, and original-context preservation. SDK and bridge tests
exercise capability and response identity separately. These are not prompt prose
or model-quality tests. Activated daemon and final user-client acceptance must still
be recorded against the final combined implementation.
