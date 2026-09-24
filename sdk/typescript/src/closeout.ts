import type {ExecutionRequest, ExecutionResponse, WorkspaceHome, WorkspaceIssueCode} from "./protocol.js";

export interface CloseoutIssue {code: WorkspaceIssueCode; detail: string}
export interface CloseoutSpec {location: string; expected_generation: number; preservation_directory: string | null; conditional_no_loss?: boolean; full_archive?: boolean}
export type CloseoutStage = "preparing" | "needs_decision" | "preserving" | "ready_for_approval" | "authorized" | "removing" | "closed" | "revoked" | "recovery_required" | "retained";
export type CloseoutRecoveryAction = "restart_preparation" | "resume_removal" | "unregister_retain_files";
export type CloseoutAuthorizationSource = {kind: "human"; client: string} | {kind: "conditional"; session: string; assessment: string};
export interface CloseoutAuthorization {review: string; seal: string; source: CloseoutAuthorizationSource}
export interface CloseoutRecord {
  operation: string; request: string; spec: CloseoutSpec; revision: number; stage: CloseoutStage; initiated_by: string;
  inventory_digest: string | null; inventory_entries: number; preservation_directory: string;
  preservation_volume_ownership?: boolean | null; authorization?: CloseoutAuthorization | null;
  preservation_digest: string | null; quarantine: string | null; removed_entries: number; issues: CloseoutIssue[];
}
export type CloseoutWorkKind = "session" | "execution" | "pending_input" | "pending_control" | "external_process" | "physical_lease" | "executor" | "unknown";
export interface CloseoutWorkFinding {kind: CloseoutWorkKind; identity: string; detail: string}
export interface CloseoutWorkReport {operation: string; observed_at: string; findings: CloseoutWorkFinding[]}
export interface CloseoutReview {
  id: string; operation: string; revision: number; inventory_digest: string | null; preservation_digest: string | null;
  references_digest: string; work: CloseoutWorkReport; issues: CloseoutIssue[]; preservation_volume_ownership: boolean | null;
}
export interface CloseoutReviewTarget {operation: string; review: string}
export interface CloseoutRecoveryPath {path: string; present: boolean; matches_recorded_root: boolean}
export interface CloseoutRecoveryReview {
  id: string; operation: string; revision: number; action: CloseoutRecoveryAction; paths: CloseoutRecoveryPath[];
  adopt_empty_holding: string | null; work: CloseoutWorkReport; issues: CloseoutIssue[];
}
export type CloseoutEntryKind = "file" | "directory" | "symlink" | "mount" | "special" | "git" | "reference";
export interface CloseoutEntry {
  id: string; path: string; kind: CloseoutEntryKind; bytes: number; sha256: string | null; link_target: string | null;
  links: number; mode: number; facts: string[]; blockers: CloseoutIssue[];
}
export type CloseoutDisposition = {kind: "retain" | "redundant"; reason: string} | {kind: "preserve"} | {kind: "preserved"; path: string};
export interface CloseoutDecision {entry: string; disposition: CloseoutDisposition; recorded_by: string}
export interface CloseoutInventoryPage {operation: string; digest: string; total: number; entries: CloseoutEntry[]; next: number | null}
export type CloseoutEntryProgress = "not_processed" | "unconfirmed" | "removed";
export interface CloseoutRemovalEntry {id: string; path: string; progress: CloseoutEntryProgress}
export interface CloseoutRemovalPage {operation: string; revision: number; total: number; completed: number; pending: CloseoutRemovalEntry | null; entries: CloseoutRemovalEntry[]; next: number | null}
export interface WorkspaceLocation {
  id: string; name: string; home: WorkspaceHome | null;
  kind: {kind:"checkout"; repository:string; origin:"managed_clone"|"adopted_git"|"linked_worktree"; common_directory:string} | {kind:"directory"} | {kind:"standalone"; git:boolean};
  observed_path:string; volume_uuid:string; binding_generation:number;
  lifecycle:"provisioning"|"ready"|"preparation_failed"|"unavailable"|"closing"|"closed"|"unregistered"; retired:boolean; revision:number;
}
export interface CloseoutHistory {location: WorkspaceLocation; operation: string; record: CloseoutRecord | null; preservation_paths: string[]; report: string | null}
export type CloseoutAction = {action:"refresh"|"preserve"|"review_removal"|"finish"} | {action:"disposition"; decision:CloseoutDecision} | {action:"approve_removal"|"apply_recovery"; review:string} | {action:"review_recovery"; choice:CloseoutRecoveryAction};
export interface CloseoutActionSpec {operation:string; expected_revision:number; action:CloseoutAction}
export type CloseoutActionResult = {kind:"record"; value:CloseoutRecord} | {kind:"review"; value:CloseoutReview} | {kind:"recovery"; value:CloseoutRecoveryReview};
export interface CloseoutActionRecord {request:string; spec:CloseoutActionSpec; initiated_by:string; run_id:string; result:CloseoutActionResult|null; issue:CloseoutIssue|null}
export type CloseoutRequest =
  | {action:"begin"; request:string; expected_revision:number; spec:CloseoutSpec}
  | {action:"inspect"|"review"|"recovery"; operation:string}
  | {action:"inventory"; operation:string; digest:string; after:number; limit:number}
  | {action:"revoke"; request:string; operation:string; expected_revision:number}
  | {action:"execute"; request:string; spec:CloseoutActionSpec}
  | {action:"inspect_action"; request:string}
  | {action:"execution"; request:string; control:ExecutionRequest}
  | {action:"removal_progress"; operation:string; expected_revision:number; after:number; limit:number}
  | {action:"history"; location:string};
export type CloseoutResponse =
  | {kind:"record"; value:CloseoutRecord} | {kind:"action"; value:CloseoutActionRecord}
  | {kind:"inventory"; value:CloseoutInventoryPage} | {kind:"review"; value:CloseoutReview|null}
  | {kind:"recovery"; value:CloseoutRecoveryReview|null} | {kind:"removal_progress"; value:CloseoutRemovalPage}
  | {kind:"history"; value:CloseoutHistory} | {kind:"execution"; value:ExecutionResponse};
export type CloseoutReply = {status:"state"; response:CloseoutResponse} | {status:"rejected"; issue:CloseoutIssue};

// Compare typed intent without depending on caller object-key insertion order.
function equal(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (a === null || b === null || typeof a !== "object" || typeof b !== "object") return false;
  if (Array.isArray(a) || Array.isArray(b)) return Array.isArray(a) && Array.isArray(b) && a.length === b.length && a.every((value, index) => equal(value, b[index]));
  const left = a as Record<string, unknown>, right = b as Record<string, unknown>;
  const keys = Object.keys(left).filter(key => left[key] !== undefined);
  return keys.length === Object.keys(right).filter(key => right[key] !== undefined).length && keys.every(key => equal(left[key], right[key]));
}

/** Structural reply correlation only. It does not grant removal authority. */
export function matchesCloseoutReply(request: CloseoutRequest, reply: CloseoutReply): boolean {
  try { return matchesReply(request, reply); } catch { return false; }
}

function matchesReply(request: CloseoutRequest, reply: CloseoutReply): boolean {
  if (reply.status === "rejected") return typeof reply.issue?.code === "string" && typeof reply.issue?.detail === "string";
  if (reply.status !== "state") return false;
  const response = reply.response;
  switch (request.action) {
    case "begin": return response.kind === "record" && response.value.request === request.request && response.value.spec.location === request.spec.location
      && response.value.spec.expected_generation === request.spec.expected_generation
      && !!response.value.spec.full_archive === !!request.spec.full_archive
      && (response.value.spec.preservation_directory ?? null) === (request.spec.preservation_directory ?? null)
      && (!response.value.spec.conditional_no_loss || !!request.spec.conditional_no_loss);
    case "inspect": case "revoke": return response.kind === "record" && response.value.operation === request.operation;
    case "execute": return response.kind === "action" && response.value.request === request.request && equal(response.value.spec, request.spec);
    case "inspect_action": return response.kind === "action" && response.value.request === request.request;
    case "inventory": return response.kind === "inventory" && response.value.operation === request.operation && response.value.digest === request.digest;
    case "review": return response.kind === "review" && (response.value === null || response.value.operation === request.operation);
    case "recovery": return response.kind === "recovery" && (response.value === null || response.value.operation === request.operation);
    case "removal_progress": return response.kind === "removal_progress" && response.value.operation === request.operation && response.value.revision === request.expected_revision;
    case "history": return response.kind === "history" && response.value.location.id === request.location;
    case "execution": {
      if (response.kind !== "execution") return false;
      const value = response.value, control = request.control;
      switch (control.action) {
        case "inspect": return value.kind === "status" && value.run.id === control.run_id;
        case "stop": return value.kind === "control" && value.run_id === control.run_id;
        case "read": return value.kind === "content" && value.run_id === control.run_id;
        case "read_part": return value.kind === "part" && value.run_id === control.run_id && value.page.part === control.part
          && value.page.offset === (control.offset ?? 0) && (control.expected_sha256 == null || value.page.sha256 === control.expected_sha256);
        default: return false;
      }
    }
  }
}
