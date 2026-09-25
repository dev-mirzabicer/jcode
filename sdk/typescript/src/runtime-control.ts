import type {WorkspaceIssueCode} from "./protocol.js";

export interface ShutdownOptions {
  strategy: "finish_current" | "interrupt";
  independent: "stop" | "keep_supported";
  quiescence_timeout_seconds: number;
}
export interface RuntimeWork { id:string; owner:string; session?:string|null; kind:"primary_turn"|"execution"|"background_task"|"preparation"; supported_survivor:boolean }
export interface ShutdownRevision { operation:string; revision:number }
export interface ShutdownReview { id:string; runtime:string; revision:number; options:ShutdownOptions; work:RuntimeWork[]; replaces?:ShutdownRevision|null }
export type ShutdownPhase = "waiting_for_current"|"stopping"|"blocked"|"stopped"|"cancelled"|"interrupted"|"superseded"|"forced";
export interface ShutdownOperation { id:string; request:string; review:ShutdownReview; revision:number; phase:ShutdownPhase; force_requested:boolean; cancellation_closed?:boolean; remaining:RuntimeWork[]; preserved:RuntimeWork[]; issues:string[] }
export interface RuntimeStatus { namespace:string; runtime?:string|null; reload_in_progress?:boolean; desired_stopped:boolean; revision:number; operation?:ShutdownOperation|null; work:RuntimeWork[] }
export type RuntimeControlRequest =
  | {action:"status"}
  | {action:"review"; options:ShutdownOptions}
  | {action:"review_change"; operation:string; expected_revision:number; options:ShutdownOptions}
  | {action:"begin"; request:string; review:string}
  | {action:"inspect"; operation:string}
  | {action:"cancel_wait"|"retry"|"force"; operation:string; expected_revision:number};
export type RuntimeControlResponse =
  | {kind:"status"; value:RuntimeStatus}
  | {kind:"review"; value:ShutdownReview}
  | {kind:"operation"; value:ShutdownOperation}
  | {kind:"error"; value:{code:WorkspaceIssueCode; detail:string}};

const object = (v:unknown): v is Record<string,unknown> => v !== null && typeof v === "object" && !Array.isArray(v);
const string = (v:unknown): v is string => typeof v === "string";
const oneOf = (v:unknown, values:string[]) => string(v) && values.includes(v);
const bool = (v:unknown): v is boolean => typeof v === "boolean";
const uint = (v:unknown): v is number => typeof v === "number" && Number.isSafeInteger(v) && v >= 0;
const optional = (v:unknown, check:(v:unknown)=>boolean) => v == null || check(v);
const keys = (v:Record<string,unknown>, names:string[]) => Object.keys(v).every(key=>names.includes(key));
const list = (v:unknown, check:(v:unknown)=>boolean) => Array.isArray(v) && v.every(check);
// UUID wire values are canonical, but callers can supply the other forms that
// Rust's UUID parser accepts. Compare identities, not their input spelling.
function uuid(v:unknown): string | undefined {
  if (!string(v)) return undefined;
  if (/^[0-9a-f]{32}$/i.test(v)) return v.toLowerCase();
  let s=v;
  if (s.startsWith("urn:uuid:")) s=s.slice(9);
  else if (s.startsWith("{") && s.endsWith("}")) s=s.slice(1,-1);
  if (/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(s)) return s.toLowerCase().replaceAll("-", "");
  return undefined;
}
const id = (v:unknown) => uuid(v) !== undefined;
const same = (a:unknown,b:unknown) => uuid(a) !== undefined && uuid(a) === uuid(b);
function options(v:unknown): v is ShutdownOptions {
  return object(v) && keys(v,["strategy","independent","quiescence_timeout_seconds"]) && oneOf(v.strategy,["finish_current","interrupt"]) && oneOf(v.independent,["stop","keep_supported"]) && uint(v.quiescence_timeout_seconds) && v.quiescence_timeout_seconds <= 0xffffffff;
}
function work(v:unknown): boolean {
  return object(v) && keys(v,["id","owner","session","kind","supported_survivor"]) && string(v.id) && string(v.owner) && optional(v.session,string) && oneOf(v.kind,["primary_turn","execution","background_task","preparation"]) && bool(v.supported_survivor);
}
function prior(v:unknown): boolean { return object(v) && keys(v,["operation","revision"]) && id(v.operation) && uint(v.revision); }
function review(v:unknown): v is ShutdownReview {
  return object(v) && keys(v,["id","runtime","revision","options","work","replaces"]) && id(v.id) && string(v.runtime) && uint(v.revision) && options(v.options) && list(v.work,work) && optional(v.replaces,prior);
}
function operation(v:unknown): v is ShutdownOperation {
  return object(v) && keys(v,["id","request","review","revision","phase","force_requested","cancellation_closed","remaining","preserved","issues"]) && id(v.id) && id(v.request) && review(v.review) && uint(v.revision) && oneOf(v.phase,["waiting_for_current","stopping","blocked","stopped","cancelled","interrupted","superseded","forced"]) && bool(v.force_requested) && (v.cancellation_closed === undefined || bool(v.cancellation_closed)) && list(v.remaining,work) && list(v.preserved,work) && list(v.issues,string);
}
function status(v:unknown): boolean {
  return object(v) && string(v.namespace) && optional(v.runtime,string) && (v.reload_in_progress === undefined || bool(v.reload_in_progress)) && bool(v.desired_stopped) && uint(v.revision) && optional(v.operation,operation) && list(v.work,work);
}
const issueCodes: WorkspaceIssueCode[] = ["preservation_incomplete","live_work","incomplete_capture","needs_grant_choice","needs_cwd","permission_required","invalid_identity","conflict","busy","offline_volume","replaced_root","corrupt_state","recovery_required","unsupported_capability","invalid_input","referenced","backup_failed","io"];
function validResponse(v:unknown): v is RuntimeControlResponse {
  if (!object(v)) return false;
  const value = v.value;
  switch (v.kind) {
    case "status": return status(v.value);
    case "review": return review(v.value);
    case "operation": return operation(v.value);
    case "error": return object(value) && string(value.code) && issueCodes.some(code=>code===value.code) && string(value.detail);
    default: return false;
  }
}

/** JSON numeric revisions outside JavaScript's exact range must not round into
 * a different authorization. Rust keeps its full u64 representation. */
export function validRuntimeRequest(v:unknown): v is RuntimeControlRequest {
  if (!object(v)) return false;
  switch (v.action) {
    case "status": return keys(v,["action"]);
    case "review": return keys(v,["action","options"]) && options(v.options);
    case "review_change": return keys(v,["action","operation","expected_revision","options"]) && id(v.operation) && uint(v.expected_revision) && options(v.options);
    case "begin": return keys(v,["action","request","review"]) && id(v.request) && id(v.review);
    case "inspect": return keys(v,["action","operation"]) && id(v.operation);
    case "cancel_wait": case "retry": case "force": return keys(v,["action","operation","expected_revision"]) && id(v.operation) && uint(v.expected_revision);
    default: return false;
  }
}
function sameOptions(a:ShutdownOptions,b:ShutdownOptions) { return a.strategy===b.strategy && a.independent===b.independent && a.quiescence_timeout_seconds===b.quiescence_timeout_seconds; }
/** Mirrors RuntimeRequest::matches_response. Not an authority or completion check. */
export function matchesRuntimeResponse(request:RuntimeControlRequest, response:unknown): response is RuntimeControlResponse {
  if (!validRuntimeRequest(request) || !validResponse(response)) return false;
  if (response.kind === "error") return true;
  switch (request.action) {
    case "status": return response.kind === "status";
    case "review": return response.kind === "review" && sameOptions(request.options,response.value.options) && response.value.replaces == null;
    case "review_change": return response.kind === "review" && sameOptions(request.options,response.value.options) && response.value.replaces != null && same(response.value.replaces.operation,request.operation) && response.value.replaces.revision===request.expected_revision;
    case "begin": return response.kind === "operation" && same(response.value.request,request.request) && same(response.value.review.id,request.review);
    case "inspect": return response.kind === "operation" && same(response.value.id,request.operation);
    case "cancel_wait": case "retry": case "force": {
      if (response.kind !== "operation" || !same(response.value.id,request.operation) || request.expected_revision >= Number.MAX_SAFE_INTEGER || response.value.revision !== request.expected_revision+1) return false;
      if (request.action === "cancel_wait") return response.value.phase === "cancelled";
      return response.value.phase === "stopping" && response.value.cancellation_closed === true && (request.action !== "force" || response.value.force_requested);
    }
  }
}
