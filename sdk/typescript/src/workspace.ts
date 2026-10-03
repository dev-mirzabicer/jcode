/**
 * Workspace catalog contracts (`workspace_catalog_v1`). Mirrors
 * `crates/jcode-workspace-types`. Correlation mirrors its `correlation.rs`;
 * `test/workspace.test.ts` evaluates the shared Rust fixture.
 */
import type {ExecutionRequest, ExecutionResponse, GrantCarryReview, LocationChangeRecord, PrimaryLaunchRecord, WorkspaceGrantDefinition, WorkspaceHome, WorkspaceIssueCode, WorkspacePlacement} from "./protocol.js";
import type {CloseoutRecord, CloseoutRequest, CloseoutResponse, WorkspaceLocation} from "./closeout.js";
import {matchesCloseoutReply} from "./closeout.js";

export interface WorkspaceIssue {code: WorkspaceIssueCode; detail: string}
export type WorkspaceEntityId = {kind: "project" | "repository" | "work_area" | "location"; id: string};
export type WorkspaceOrganizationState = "active" | "archived" | "retired";
export interface WorkspaceProject {id: string; name: string; state: WorkspaceOrganizationState; revision: number}
export interface WorkspaceRepository {id: string; name: string; remotes: string[]; state: WorkspaceOrganizationState; revision: number}
export interface WorkspaceArea {id: string; project: string; name: string; state: WorkspaceOrganizationState; revision: number}
export type WorkspaceEntity =
  | {kind: "project"; value: WorkspaceProject} | {kind: "repository"; value: WorkspaceRepository}
  | {kind: "work_area"; value: WorkspaceArea} | {kind: "location"; value: WorkspaceLocation};
export type WorkspaceRegistration = {kind: "checkout"; home: WorkspaceHome; repository: string} | {kind: "directory"; home: WorkspaceHome} | {kind: "standalone"};
export type WorkspaceOrganizationChange =
  | {action: "create_project"; name: string}
  | {action: "create_repository"; name: string; remotes: string[]}
  | {action: "associate_repository" | "remove_repository_association"; project: string; repository: string}
  | {action: "create_work_area"; project: string; name: string}
  | {action: "register_location"; name: string; path: string; registration: WorkspaceRegistration}
  | {action: "rebind_location"; location: string; expected_old_path: string; expected_generation: number; new_path: string}
  | {action: "move_location"; location: string; home: WorkspaceHome; associate_repository: boolean}
  | {action: "adopt_standalone"; location: string; home: WorkspaceHome; repository: string | null; associate_repository: boolean}
  | {action: "rename"; target: WorkspaceEntityId; name: string}
  | {action: "archive"; target: WorkspaceEntityId; archived: boolean}
  | {action: "retire" | "discard_unused"; target: WorkspaceEntityId}
  | {action: "set_volume_default"; volume_uuid: string; path: string};
export type WorkspaceEntityKind = "project" | "repository" | "work_area" | "location";
export type WorkspaceVisibility = "current" | "all" | "archived" | "retired" | "closed";
export interface WorkspaceQuery {kind: WorkspaceEntityKind | null; project: string | null; home: WorkspaceHome | null; repository: string | null; visibility: WorkspaceVisibility; active_sessions_only: boolean}
export interface WorkspaceCursor {revision: number; after: string; query_digest: string}
export interface WorkspacePage {revision: number; total: number; items: WorkspaceEntity[]; next: WorkspaceCursor | null}
export interface WorkspaceCatalogStatus {installation: string; schema: number; revision: number; managed_rollout: boolean}
export interface WorkspaceReview {id: string; revision: number; change: WorkspaceOrganizationChange; targets: WorkspaceEntityId[]; issues: WorkspaceIssue[]}
export interface WorkspaceReceipt {operation: string; request: string; revision: number; targets: WorkspaceEntityId[]; issues: WorkspaceIssue[]}
export interface WorkspaceSessionIndex {session: string; placement: WorkspacePlacement; session_revision: number; operation: string; active: boolean; reconciled: boolean}
export interface WorkspaceSnapshot {id: string; name: string; path: string; sha256: string; revision: number; automatic: boolean}
export interface WorkspaceLocationRemap {location: string; path: string}
export interface WorkspaceImportReview {
  id: string; revision: number; source_installation: string; collisions: WorkspaceEntityId[];
  revision_differences: {identity: WorkspaceEntityId; current: number; incoming: number}[];
  entities: WorkspaceEntity[]; remapped: string[]; unavailable: string[]; disabled_grants: string[]; external_content: string[]; issues: WorkspaceIssue[];
}
export interface WorkspaceRestoreReview {id: string; snapshot: WorkspaceSnapshot; current_revision: number | null; grants: WorkspaceGrantDefinition[]; issues: WorkspaceIssue[]}
export interface WorkspaceVolume {uuid: string; mount: string; label: string; internal: boolean; writable: boolean; available_bytes: number}

export interface CloneSpec {
  home: WorkspaceHome; repository: string; name: string;
  source: {kind: "remote"; url: string} | {kind: "local"; path: string};
  base: {kind: "branch" | "tag"; name: string} | {kind: "commit"; oid: string};
  branch: {kind: "keep_name" | "detached"} | {kind: "create"; name: string};
  remotes: {name: string; url: string}[];
  destination: {kind: "default"; volume_uuid: string; project_component: string; checkout_component: string} | {kind: "custom"; volume_uuid: string; path: string};
  submodules: boolean; lfs: boolean; trusted_submodule_urls?: string[]; trusted_lfs_urls?: string[];
}
export interface CloneReview {id: string; revision: number; spec: CloneSpec; source_commit: string; destination: string; volume_uuid: string; issues: WorkspaceIssue[]}
export type CloneState = "pending" | "acquiring" | "materializing" | "awaiting_trust" | "verifying" | "publishing" | "ready" | "preparation_failed" | "cancelled" | "recovery_required";
export interface CloneTrustSource {kind: "submodule" | "lfs"; repository: string; path: string; url: string}
export interface CloneTrustReview {id: string; clone: string; catalog_revision: number; clone_revision: number; stage: string; source_commit: string; sources: CloneTrustSource[]}
export interface CloneRecord {
  operation: string; request: string; location: string; review: CloneReview; state: CloneState; cancel_requested: boolean; stage: string | null;
  output_runs: string[]; discovered_sources?: CloneTrustSource[]; pending_trust?: CloneTrustSource[];
  trust_approvals?: {request: string; review: string; issued_by: string; sources: CloneTrustSource[]}[];
  output_issue?: WorkspaceIssue | null; issue: WorkspaceIssue | null; revision: number;
}
export interface RebindRecord {operation: string; location: string; old_path: string; new_path: string; old_volume_uuid: string; new_volume_uuid: string; old_generation: number; new_generation: number}
export interface StartupCopyApproval {source_spec_id: string; approved_resolved_target: string}
export interface StartupCopyReview {
  id: string; catalog_revision: number; source: string; target: string; target_path: string; target_binding_generation: number;
  source_plan_revision: number; target_plan_revision: number; proposed_plan_revision: number;
  entries: {source_spec_id: string; selected_path: string; resolved_target: string; external: boolean}[];
}
export interface StartupCopyRecord {request: string; operation: string; review: StartupCopyReview; state: "pending" | "complete" | "recovery_required"; issue: WorkspaceIssue | null}
export interface StartupCopyPlans {catalog_revision: number; source_revision: number; target_revision: number; source_entries: number}

export type OperationKind = "clone" | "closeout" | "startup_copy" | "primary_launch" | "primary_location";
export interface OperationQuery {target?: WorkspaceEntityId | null; session?: string | null; kinds?: OperationKind[]; unfinished_only?: boolean}
export type WorkspaceOperation =
  | {kind: "clone"; record: CloneRecord} | {kind: "closeout"; record: CloseoutRecord} | {kind: "startup_copy"; record: StartupCopyRecord}
  | {kind: "primary_launch"; record: PrimaryLaunchRecord} | {kind: "primary_location"; record: LocationChangeRecord};
export interface OperationEntry {state: "pending" | "complete" | "failed" | "recovery_required"; targets: WorkspaceEntityId[]; operation: WorkspaceOperation}
export interface OperationPage {revision: number; total: number; items: OperationEntry[]; next: WorkspaceCursor | null}

export type WorkspaceAudience = WorkspaceGrantDefinition["audience"];
export type WorkspaceWriteTarget = WorkspaceGrantDefinition["target"];
export type AccessProposalState = "pending" | "approved" | "declined" | "cancelled";
export interface AccessProposal {id: string; session: string; target: WorkspaceWriteTarget; revision: number; state?: AccessProposalState; reason?: string; grant?: string | null}
export type GrantChange =
  | {action: "bind_imported"; reference: string; installation: string; grant: string; audience: WorkspaceAudience; target: WorkspaceWriteTarget}
  | {action: "issue"; audience: WorkspaceAudience; target: WorkspaceWriteTarget; proposal: string | null}
  | {action: "activate_imported" | "revoke"; grant: string};
export interface GrantReview {id: string; revision: number; change: GrantChange; grant: WorkspaceGrantDefinition; roots: WorkspaceLocation[]; excluded_roots?: WorkspaceLocation[]}
export interface PermissionMutation {receipt: WorkspaceReceipt; grant: WorkspaceGrantDefinition | null; proposal: AccessProposal | null}
export interface WritableRoot {location: WorkspaceLocation; ordinary: boolean; grants: string[]; issue: WorkspaceIssue | null}
export interface SessionWriteScope {session: string; placement: WorkspacePlacement; session_revision: number; catalog_revision: number; roots: WritableRoot[]; grants: WorkspaceGrantDefinition[]}
export interface ContextScopeStatus {review: string | null; operation: string; source: string; target: string; kind: "split" | "clear" | "transfer"; state: "pending" | "complete" | "failed" | "recovery_required"; revision: number | null; backup_pending: boolean}
export type PermissionQuery = {kind: "grants"; audience: WorkspaceAudience | null} | {kind: "proposals"; session: string | null; state: AccessProposalState | null};
export type PermissionItem = {kind: "grant"; value: WorkspaceGrantDefinition} | {kind: "proposal"; value: AccessProposal};
export interface ImportedGrantReference {reference: string; installation: string; grant: WorkspaceGrantDefinition; bound_grant: string | null}
export type PermissionRequest =
  | {action: "decide_proposal"; request: string; proposal: string; expected_revision: number; decision: "decline" | "cancel"}
  | {action: "imported_grants"; after: WorkspaceCursor | null; limit: number}
  | {action: "abandon_context_scope" | "reconcile_context_scope" | "review_carry" | "scope"; session: string}
  | {action: "context_scope_status"; review: string}
  | {action: "review"; expected_revision: number; change: GrantChange}
  | {action: "apply"; request: string; review: string}
  | {action: "grant"; grant: string}
  | {action: "propose"; session: string; request: string; target: WorkspaceWriteTarget; reason: string}
  | {action: "proposal"; proposal: string}
  | {action: "list"; query: PermissionQuery; after: WorkspaceCursor | null; limit: number};
export type PermissionResponse =
  | {kind: "imported_grants"; value: {revision: number; total: number; items: ImportedGrantReference[]; next: WorkspaceCursor | null}}
  | {kind: "context_scopes"; value: ContextScopeStatus[]} | {kind: "carry_review"; value: GrantCarryReview}
  | {kind: "scope"; value: SessionWriteScope} | {kind: "review"; value: GrantReview} | {kind: "mutation"; value: PermissionMutation}
  | {kind: "grant"; value: WorkspaceGrantDefinition} | {kind: "proposal"; value: AccessProposal}
  | {kind: "page"; value: {revision: number; total: number; items: PermissionItem[]; next: WorkspaceCursor | null}};

export type WorkspaceRequest =
  | {action: "closeout"; request: CloseoutRequest}
  | {action: "volumes" | "status" | "snapshots"}
  | {action: "clone_output"; clone: string; request: ExecutionRequest}
  | {action: "review_clone"; expected_revision: number; spec: CloneSpec}
  | {action: "begin_clone" | "apply_clone_trust" | "apply_startup_copy" | "apply" | "apply_import" | "apply_restore"; request: string; review: string}
  | {action: "inspect_clone" | "cancel_clone" | "resume_clone" | "inspect_startup_copy" | "initialize" | "inspect_receipt"; request: string}
  | {action: "review_clone_trust"; clone: string; expected_revision: number}
  | {action: "inspect_rebind"; operation: string}
  | {action: "review_startup_copy"; expected_catalog_revision: number; source: string; target: string; expected_source_plan_revision: number; expected_target_plan_revision: number; external_approvals: StartupCopyApproval[]}
  | {action: "startup_copy_plans"; source: string; target: string}
  | {action: "operations"; query: OperationQuery; after: WorkspaceCursor | null; limit: number}
  | {action: "permissions"; request: PermissionRequest}
  | {action: "list"; query: WorkspaceQuery; after: WorkspaceCursor | null; limit: number}
  | {action: "inspect"; target: WorkspaceEntityId}
  | {action: "review"; expected_revision: number; change: WorkspaceOrganizationChange}
  | {action: "sessions"; target: WorkspaceEntityId | null; after: string | null; limit: number}
  | {action: "backup"; request: string; name: string}
  | {action: "export"; request: string; project: string; name: string}
  | {action: "review_import"; path: string; expected_revision: number; collisions: "reject" | "new_identities"; remap: WorkspaceLocationRemap[]}
  | {action: "review_restore"; snapshot: string};
export type WorkspaceResponse =
  | {kind: "closeout"; value: CloseoutResponse} | {kind: "volumes"; value: WorkspaceVolume[]}
  | {kind: "clone_output"; value: ExecutionResponse} | {kind: "clone_review"; value: CloneReview} | {kind: "clone"; value: CloneRecord}
  | {kind: "clone_trust_review"; value: CloneTrustReview} | {kind: "rebind"; value: RebindRecord}
  | {kind: "startup_copy_review"; value: StartupCopyReview} | {kind: "startup_copy"; value: StartupCopyRecord}
  | {kind: "startup_copy_plans"; value: StartupCopyPlans} | {kind: "operations"; value: OperationPage}
  | {kind: "permissions"; value: PermissionResponse} | {kind: "status"; value: WorkspaceCatalogStatus}
  | {kind: "page"; value: WorkspacePage} | {kind: "entity"; value: WorkspaceEntity} | {kind: "review"; value: WorkspaceReview}
  | {kind: "receipt"; value: WorkspaceReceipt} | {kind: "sessions"; value: WorkspaceSessionIndex[]}
  | {kind: "snapshot"; value: WorkspaceSnapshot} | {kind: "snapshots"; value: WorkspaceSnapshot[]} | {kind: "export"; value: string}
  | {kind: "import_review"; value: WorkspaceImportReview} | {kind: "restore_review"; value: WorkspaceRestoreReview}
  | {kind: "error"; value: WorkspaceIssue};

export type WorkspaceCapability = "catalog" | "permissions" | "checkout" | "closeout" | "management";
export interface WorkspaceVersions {catalog_version: number; permissions_version?: number | null; checkout_version?: number | null; closeout_version?: number | null; management_version?: number | null; managed_rollout: boolean}

/** Only the exact native versions these contracts describe are supported. */
export function workspaceSupports(versions: WorkspaceVersions, capability: WorkspaceCapability): boolean {
  switch (capability) {
    case "catalog": return versions.catalog_version === 1;
    case "permissions": return versions.permissions_version === 1;
    case "checkout": return versions.checkout_version === 1;
    case "closeout": return versions.closeout_version === 2;
    case "management": return versions.management_version === 1;
  }
}

export function requiredWorkspaceCapability(request: WorkspaceRequest): WorkspaceCapability {
  switch (request.action) {
    case "closeout": return "closeout";
    case "permissions": return "permissions";
    case "volumes": case "clone_output": case "review_clone": case "begin_clone": case "inspect_clone": case "cancel_clone": case "resume_clone":
    case "review_clone_trust": case "apply_clone_trust": case "inspect_rebind": case "review_startup_copy": case "apply_startup_copy": case "inspect_startup_copy":
      return "checkout";
    case "startup_copy_plans": case "operations": return "management";
    default: return "catalog";
  }
}

function equal(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (a === null || b === null || typeof a !== "object" || typeof b !== "object") return false;
  if (Array.isArray(a) || Array.isArray(b)) return Array.isArray(a) && Array.isArray(b) && a.length === b.length && a.every((value, index) => equal(value, b[index]));
  const left = a as Record<string, unknown>, right = b as Record<string, unknown>;
  const keys = Object.keys(left).filter(key => left[key] !== undefined && left[key] !== null);
  return keys.length === Object.keys(right).filter(key => right[key] !== undefined && right[key] !== null).length && keys.every(key => equal(left[key], right[key]));
}

function executionMatches(control: ExecutionRequest, reply: ExecutionResponse): boolean {
  const value = reply as Record<string, any>, request = control as Record<string, any>;
  switch (request.action) {
    case "inspect": return value.kind === "status" && value.run?.id === request.run_id;
    case "stop": return value.kind === "control" && value.run_id === request.run_id;
    case "read": return value.kind === "content" && value.run_id === request.run_id;
    case "read_part": return value.kind === "part" && value.run_id === request.run_id && value.page?.part === request.part
      && value.page?.offset === (request.offset ?? 0) && (request.expected_sha256 == null || value.page?.sha256 === request.expected_sha256);
    default: return false;
  }
}

function entityId(entity: WorkspaceEntity): WorkspaceEntityId {
  return {kind: entity.kind, id: entity.value.id};
}

/** Structural reply correlation only. A match never confers authority. */
export function matchesWorkspaceResponse(request: WorkspaceRequest, response: WorkspaceResponse): boolean {
  try { return matches(request, response); } catch { return false; }
}

function matches(request: WorkspaceRequest, response: WorkspaceResponse): boolean {
  if (response.kind === "error") return typeof response.value?.code === "string" && typeof response.value?.detail === "string";
  const r = response;
  switch (request.action) {
    case "closeout": return r.kind === "closeout" && matchesCloseoutReply(request.request, {status: "state", response: r.value});
    case "volumes": return r.kind === "volumes";
    case "clone_output": return r.kind === "clone_output" && executionMatches(request.request, r.value);
    case "review_clone": return r.kind === "clone_review" && equal(r.value.spec.home, request.spec.home) && r.value.spec.repository === request.spec.repository && r.value.revision >= request.expected_revision;
    case "begin_clone": case "inspect_clone": case "cancel_clone": case "resume_clone": return r.kind === "clone" && r.value.request === request.request;
    case "review_clone_trust": return r.kind === "clone_trust_review" && r.value.clone === request.clone;
    case "apply_clone_trust": case "apply": case "inspect_receipt": case "apply_import": case "apply_restore": return r.kind === "receipt" && r.value.request === request.request;
    case "inspect_rebind": return r.kind === "rebind" && r.value.operation === request.operation;
    case "review_startup_copy": return r.kind === "startup_copy_review" && r.value.target === request.target;
    case "apply_startup_copy": case "inspect_startup_copy": return r.kind === "startup_copy" && r.value.request === request.request;
    case "startup_copy_plans": return r.kind === "startup_copy_plans";
    case "operations": return r.kind === "operations" && r.value.items.length <= request.limit;
    case "permissions": return r.kind === "permissions" && permissionMatches(request.request, r.value);
    case "status": case "initialize": return r.kind === "status";
    case "list": return r.kind === "page" && r.value.items.length <= request.limit;
    case "inspect": return r.kind === "entity" && equal(entityId(r.value), request.target);
    case "review": return r.kind === "review" && r.value.change.action === request.change.action && r.value.revision === request.expected_revision;
    case "sessions": return r.kind === "sessions" && r.value.length <= request.limit;
    case "backup": return r.kind === "snapshot" && r.value.name === request.name;
    case "snapshots": return r.kind === "snapshots";
    case "export": return r.kind === "export";
    case "review_import": return r.kind === "import_review";
    case "review_restore": return r.kind === "restore_review" && r.value.snapshot.id === request.snapshot;
  }
}

function permissionMatches(request: PermissionRequest, response: PermissionResponse): boolean {
  const r = response;
  switch (request.action) {
    case "decide_proposal": return r.kind === "mutation" && r.value.receipt.request === request.request && r.value.proposal?.id === request.proposal;
    case "imported_grants": return r.kind === "imported_grants" && r.value.items.length <= request.limit;
    case "abandon_context_scope": case "reconcile_context_scope": return r.kind === "context_scopes" && r.value.every(scope => scope.source === request.session || scope.target === request.session);
    case "context_scope_status": return r.kind === "context_scopes" && r.value.every(scope => scope.review == null || scope.review === request.review);
    case "review_carry": return r.kind === "carry_review" && r.value.source === request.session;
    case "scope": return r.kind === "scope" && r.value.session === request.session;
    case "review": return r.kind === "review" && equal(r.value.change, request.change);
    case "apply": return r.kind === "mutation" && r.value.receipt.request === request.request;
    case "grant": return r.kind === "grant" && r.value.id === request.grant;
    case "propose": return r.kind === "mutation" && r.value.receipt.request === request.request && r.value.proposal != null
      && r.value.proposal.session === request.session && equal(r.value.proposal.target, request.target);
    case "proposal": return r.kind === "proposal" && r.value.id === request.proposal;
    case "list": return r.kind === "page" && r.value.items.length <= request.limit;
  }
}
