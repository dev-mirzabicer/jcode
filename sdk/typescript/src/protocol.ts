/**
 * Wire types for the jcode harness API (protocol v1).
 *
 * Mirrors `crates/jcode-harness-api` exactly: request tags live under `req`,
 * event tags under `ev`, and every frame carries `v`. Keep this file in sync
 * with the Rust enums; `test/schema-parity.test.ts` fails the build if the
 * tag sets drift apart.
 */

export const API_VERSION_MAJOR = 1;
export const API_VERSION_MINOR = 8;

export type WorkspacePlacement = { kind: "project" | "work_area" | "checkout" | "directory" | "standalone"; id: string };
export type WorkspaceHome = { kind: "project" | "work_area"; id: string };
export interface PrimaryModel { model: string; provider: string; api_method: string; effort: string | null }
export interface PrimaryLaunchInput {
  placement: { kind: "existing"; placement: WorkspacePlacement } | { kind: "standalone"; root: string };
  cwd: {kind: "existing"; path: string} | {kind: "create_empty"; path: string; home: WorkspaceHome | null} | null;
  agent: string | null; model: PrimaryModel | null; selfdev?: boolean;
}
export interface PrimaryLaunchRequest { request: string; expected_revision: number; input: PrimaryLaunchInput }
export interface PrimaryLaunchRecord {
  request: string; operation: string; session: string; input: PrimaryLaunchInput; concrete_model: PrimaryModel;
  registration_request: string; state: "pending" | "complete" | "failed" | "recovery_required";
  reviewed_revision: number; published_revision: number | null; issue: string | null; backup_pending: boolean;
}
export type WorkspaceIssueCode = "needs_cwd" | "permission_required" | "invalid_identity" | "conflict" | "busy" | "offline_volume" | "replaced_root" | "corrupt_state" | "recovery_required" | "unsupported_capability" | "invalid_input" | "referenced" | "backup_failed" | "io";
export type PrimaryLaunchResponse = {status: "launched"; record: PrimaryLaunchRecord} | {status: "rejected"; request: string; issue: {code: WorkspaceIssueCode; detail: string}};

export interface PrimaryInputEnvelope {
  activate_skill?:string|null; observe_startup_context?:boolean|null; client_request_digest?:string|null;
  id: string; session: string; delivery: "safe_boundary" | "next_turn" | "context_only"; content: string;
  images?: [string, string][]; urgent?: boolean; display_role?: "system" | "background_task" | null;
  origin?: {kind: "human"} | {kind: "composed"; parts: {start:number; end:number; notice:"long_review"|"intent"|"feedback_loop"|"ownership"|"completion"|"confidence"|"digest"|"incomplete"|null; incomplete_count?:number}[]} | null;
  system_reminder?: string | null;
  unattended_context?: {policy: {mode:"block"} | {mode:"authorized"; protected_recent_assistant_turns:number; target_headroom_percent:number; allow_reasoning_suppression:boolean; allow_tool_distillation:boolean; allow_oldest_range_summary:boolean; authorization_source:string}; authorization_source:string; scheduled_item_id?:string} | null;
}
export interface PrimaryInputReceipt { id:string; session:string; state:"accepted"|"committed"|"failed"|"cancelled"; messages:string[]; issue:string|null }
export interface LocationChangeRequest { request:string; session:string; expected_session_revision:number; expected_catalog_revision:number; placement:WorkspacePlacement; cwd:string }
export interface LegacyLocationAdoptionRequest { request:string; session:string; expected_working_dir:string|null; expected_catalog_revision:number; placement:WorkspacePlacement; cwd:string }
export type PrimaryLocationCommand = {action:"change"; request:LocationChangeRequest} | {action:"adopt_legacy"; request:LegacyLocationAdoptionRequest} | {action:"inspect"|"cancel"; operation:string};
export interface LocationChangeRecord { legacy_origin?:{working_dir:string|null}|null; operation:string; input:LocationChangeRequest; state:"pending"|"complete"|"cancelled"|"failed"|"recovery_required"; effective_revision:number|null; notice_message:string|null; issue:{code:WorkspaceIssueCode; detail:string}|null }
export type PrimaryLocationResponse = {status:"state"; record:LocationChangeRecord} | {status:"rejected"; issue:{code:WorkspaceIssueCode; detail:string}};

export type ExecutionState = "prepared" | "queued" | "running" | "completed" | "failed" | "cancelled" | "interrupted";
export type OutputSize = number | "very_small" | "small" | "medium" | "large" | "very_large";
export interface ExecutionRun {
  superseded?: boolean;
  id: string; session_id: string; message_id: string; tool: string;
  state: ExecutionState; owner: string; input_path: string;
  result_path: string | null; output_path: string | null; output_bytes: number;
  complete: boolean; background: boolean; parent_id: string | null;
  stop_cause: "human_cancellation" | "parent_foreground_cancellation" | "child_predecessor_failure" | "reload_quiescence" | "owner_crash" | null;
  process_exit?: {code: number | null; signal: number | null; timed_out: boolean};
  progress?: {value: Record<string, unknown>; checkpoint: boolean; sequence: number};
}
export interface ExecutionPage {
  superseded?: boolean;
  output: string; title: string | null; metadata: unknown;
  images: {media_type: string; data: string; label: string | null}[];
  resources: {uri: string; media_type: string | null; data: string}[];
  source: {kind: string; [key: string]: unknown}; is_error: boolean;
}
export interface ExecutionPartPage {
  part: string; offset: number; total_bytes: number; sha256: string;
  data_base64: string; next_offset: number | null;
}
export type ExecutionRequest =
  | { action: "read_part"; run_id: string; part: string; offset?: number | null; limit?: number | null; expected_sha256?: string | null }
  | {action: "list"; all_sessions?: boolean; after?: string | null; limit?: number | null}
  | {action: "inspect" | "stop" | "force_stop" | "background"; run_id: string}
  | {action: "read"; run_id: string; content: "input" | "output"; read_point?: string | null; output_size?: OutputSize | null};
export type ExecutionResponse =
  | { kind: "part"; run_id: string; page: ExecutionPartPage }
  | {kind: "list"; runs: ExecutionRun[]; next: string | null}
  | {kind: "status"; run: ExecutionRun}
  | {kind: "control"; run_id: string; accepted: boolean; state: ExecutionState}
  | {kind: "content"; run_id: string; page: ExecutionPage};

export type PermissionDecision = "allow" | "allow_always" | "deny";

export type InspectionRequest =
  | {action: "outline"; target: string; output_size?: OutputSize | null}
  | {action: "transcript"; snapshot_id: string; range?: {start: number; end: number} | null; raw?: boolean; output_size?: OutputSize | null}
  | {action: "expand_tool"; snapshot_id: string; tool_use_id: string; output_size?: OutputSize | null};
export interface InspectionResponse {snapshot_id: string; content: ExecutionPage}
export type CleanupSelection = {selection: "oldest_bytes"; bytes: number} | {selection: "outputs"; run_ids: string[]};
export interface CleanupCandidate {run_id: string; session_id: string; bytes: number; created_at: number; affected_snapshot_ids: string[]}
export interface CleanupReview {review_id: string; confirmation_id: string; candidates: CleanupCandidate[]; requested_bytes: number | null; selected_bytes: number; overshoot_bytes: number; impact: string}
export interface CleanupOutcome {review_id: string; items: {run_id: string; deleted: boolean; error: string | null}[]}
export interface RetentionReport {checked_at: number; archived_outputs: number; pruned_snapshots: number; resumed_cleanups: number; issues: {id: string; message: string}[]}
export type CleanupRequest = {action: "status"} | {action: "review"; selection: CleanupSelection} | {action: "confirm"; review_id: string; confirmation_id: string};
export type CleanupResponse = {kind: "status"; status: RetentionReport | null} | {kind: "review"; review: CleanupReview} | {kind: "outcome"; outcome: CleanupOutcome};

export type ErrorCode =
  | "unsupported_version"
  | "unknown_request"
  | "unknown_session"
  | "invalid_request"
  | "internal";

export interface SessionInfo {
  session_id: string;
  /** False for isolated children. Missing means eligibility is not known. */
  attachable?: boolean;
  working_dir?: string;
  title?: string;
  status: string;
  /** Approximate size of the stored transcript, in bytes. */
  transcript_bytes?: number;
  archived?: boolean;
  archived_at_ms?: number;
}

export interface ModelRouteInfo {
  model: string;
  provider: string;
  api_method: string;
  available: boolean;
  detail: string;
}

export interface TextMatch {
  path: string;
  line: number;
  column: number;
  preview: string;
}

export interface HistoryMessage {
  /** "user" | "assistant" | "tool" */
  role: string;
  content: string;
}

export interface AgentInfo {
  agent_id: string;
  display_name: string;
  scope: string;
  description: string;
  active: boolean;
}

export type StartupContextCreateErrorKind =
  | "unsupported"
  | "project_identity"
  | "plan_storage"
  | "invalid_files"
  | "persistence"
  | "internal";

export interface StartupContextCreateIssue {
  logical_path?: string;
  code: string;
  detail: string;
}

export interface StartupContextCreateError {
  kind: StartupContextCreateErrorKind;
  message: string;
  issues?: StartupContextCreateIssue[];
}

/** Base64 image attachment: [mediaType, base64Data]. */
export type ImageAttachment = [string, string];

export type WorkflowLoopMode = "improve_run" | "improve_plan" | "refactor_run" | "refactor_plan";
export interface WorkflowTodo { content: string; status: string; priority: string }
export type CommandWorkflow =
  | { kind: "commit" | "commit_push" | "release_fast" | "release_macos" | "release_remote" | "improve_stop" | "refactor_stop" }
  | { kind: "triage"; focus: string }
  | { kind: "test"; claim: string }
  | { kind: "plan"; goal: string | null }
  | { kind: "improve" | "refactor"; plan_only: boolean; focus: string | null }
  | { kind: "improve_resume" | "refactor_resume"; mode: WorkflowLoopMode; todos: WorkflowTodo[] };

export type WorkflowPromptRequest =
  | { kind: "command"; command: CommandWorkflow }
  | { kind: "review_startup"; mode: "review" | "autoreview" | "judge" | "autojudge"; parent_session_id: string }
  | { kind: "structured_initial"; content: string; schema: string }
  | { kind: "structured_correction"; schema: string; error_lines: string; previous_response: string };

export type ApiRequest =
  | {req: "primary_control_probe"}
  | {req: "primary_input"; input:PrimaryInputEnvelope}
  | {req: "primary_input_inspect"; session:string; input:string}
  | {req: "primary_input_read"; session:string; input:string}
  | {req: "primary_location"; command:PrimaryLocationCommand}
  | {req: "primary_launch_probe"}
  | {req: "primary_launch"; request: PrimaryLaunchRequest}
  | {req: "execution"; session_id: string; request: ExecutionRequest}
  | {req: "session_inspection"; session_id: string; request: InspectionRequest}
  | {req: "output_cleanup"; session_id: string; request: CleanupRequest}
  | { req: "hello"; min_version: number; max_version: number; client: string }
  | { req: "list_sessions"; include_archived?: boolean }
  | { req: "archive_session"; session_id: string }
  | { req: "restore_session"; session_id: string }
  | { req: "set_retention_policy"; archive_after_days?: number }
  | { req: "create_session"; working_dir?: string; agent?: string }
  | { req: "attach_session"; session_id: string }
  | { req: "detach_session"; session_id: string }
  | {
      req: "send_message";
      session_id: string;
      content: string;
      images?: ImageAttachment[];
      no_reply?: boolean;
    }
  | { req: "cancel"; session_id: string }
  | {
      req: "soft_interrupt";
      session_id: string;
      content: string;
      urgent?: boolean;
    }
  | { req: "get_history"; session_id: string }
  | { req: "list_agents"; session_id: string }
  | { req: "set_agent"; session_id: string; agent: string; replace?: boolean }
  | { req: "render_workflow_prompt"; session_id: string; workflow: WorkflowPromptRequest }
  | { req: "inspect_agent"; session_id: string; include_instructions?: boolean }
  | { req: "peek_session"; session_id: string; limit?: number }
  | { req: "clear"; session_id: string }
  | { req: "rewind"; session_id: string; message_index: number }
  | {
      req: "permission_response";
      session_id: string;
      request_id: string;
      decision: PermissionDecision;
    }
  | { req: "list_models"; session_id: string }
  | { req: "get_runtime_info"; session_id: string }
  | { req: "set_api_key"; provider: string; api_key: string }
  | { req: "clear_api_key"; provider: string }
  | { req: "read_file"; session_id: string; path: string; max_bytes?: number }
  | { req: "find_files"; session_id: string; query: string; limit?: number }
  | { req: "search_text"; session_id: string; query: string; path?: string; limit?: number }
  | { req: "file_status"; session_id: string; path: string }
  | { req: "set_model"; session_id: string; model: string }
  | { req: "set_reasoning_effort"; session_id: string; effort: string }
  | { req: "rename_session"; session_id: string; title?: string }
  | { req: "rewind_undo"; session_id: string }
  | { req: "cancel_soft_interrupts"; session_id: string }
  | { req: "ping" };

export type ApiEvent =
  | {ev: "primary_control_capabilities"; input_version:number; location_version:number; location_enabled:boolean; legacy_adoption_version?:number|null}
  | {ev: "primary_input_receipt"; receipt:PrimaryInputReceipt}
  | {ev: "primary_input_detail"; receipt:PrimaryInputReceipt; input:PrimaryInputEnvelope}
  | {ev: "primary_location"; response:PrimaryLocationResponse}
  | {ev: "primary_launch_capabilities"; version: number; enabled: boolean}
  | {ev: "primary_launch"; response: PrimaryLaunchResponse}
  | {ev: "execution"; session_id: string; response: ExecutionResponse}
  | {ev: "session_inspection"; session_id: string; response: InspectionResponse}
  | {ev: "output_cleanup"; session_id: string; response: CleanupResponse}
  | { ev: "workflow_prompt_rendered"; session_id: string; content: string }
  | { ev: "hello_ok"; version: number; server: string; capabilities?: string[] }
  | { ev: "ok" }
  | { ev: "error"; code: ErrorCode; message: string }
  | { ev: "sessions"; sessions: SessionInfo[] }
  | { ev: "attached"; session: SessionInfo }
  | { ev: "startup_context_creation_failed"; error: StartupContextCreateError }
  | { ev: "history"; session_id: string; messages: HistoryMessage[] }
  | { ev: "agents"; session_id: string; agents: AgentInfo[] }
  | {
      ev: "agent_changed";
      session_id: string;
      agent_id: string;
      display_name: string;
      scope: string;
      change: string;
      message_id?: string;
    }
  | {
      ev: "agent_status";
      session_id: string;
      agent_id: string;
      display_name: string;
      scope: string;
      first_provider_dispatched: boolean;
      active_transition_message_id?: string;
      system_prompt?: string;
      active_skill?: string;
    }
  | { ev: "pong" }
  | { ev: "text_delta"; session_id: string; text: string }
  | { ev: "reasoning_delta"; session_id: string; text: string }
  | { ev: "reasoning_done"; session_id: string; duration_secs?: number }
  | { ev: "tool_start"; session_id: string; call_id: string; name: string }
  | { ev: "tool_input_delta"; session_id: string; call_id: string; delta: string }
  | { ev: "tool_exec"; session_id: string; call_id: string; name: string }
  | {
      ev: "tool_done";
      session_id: string;
      call_id: string;
      name: string;
      output: string;
      error?: string;
    }
  | {
      ev: "token_usage";
      session_id: string;
      input: number;
      output: number;
      cache_read_input?: number;
    }
  | { ev: "turn_done"; session_id: string }
  | {
      ev: "background_progress";
      session_id: string;
      task_id: string;
      label: string;
      percent?: number;
      summary: string;
      done?: boolean;
    }
  | { ev: "message_accepted"; session_id: string }
  | {
      ev: "permission_request";
      session_id: string;
      request_id: string;
      tool_name: string;
      description: string;
    }
  | { ev: "session_status"; session_id: string; status: string }
  | { ev: "model_info"; session_id: string; provider?: string; model?: string }
  | { ev: "models"; session_id: string; models: string[]; current?: string }
  | {
      ev: "runtime_info";
      session_id: string;
      provider?: string;
      model?: string;
      routes: ModelRouteInfo[];
    }
  | { ev: "credential_updated"; provider: string; configured: boolean }
  | {
      ev: "file_content";
      session_id: string;
      path: string;
      content: string;
      size: number;
      truncated: boolean;
    }
  | { ev: "files"; session_id: string; paths: string[] }
  | { ev: "text_matches"; session_id: string; matches: TextMatch[] }
  | {
      ev: "file_status";
      session_id: string;
      path: string;
      exists: boolean;
      kind: string;
      size?: number;
      modified_ms?: number;
    }
  | {
      ev: "session_renamed";
      session_id: string;
      title?: string;
      display_title: string;
    };

/**
 * An event kind this SDK does not know about.
 *
 * The harness may add events at any time within protocol v1, so one can arrive
 * at runtime. It is deliberately *not* part of `ApiEvent`: a member with
 * `ev: string` widens the discriminant, and TypeScript then refuses to narrow
 * `event.ev === "text_delta"` to the text-delta member, leaving every field
 * typed `unknown`. Forward compatibility is a runtime property, and paying for
 * it with the type safety of the ninety-nine percent case is a bad trade.
 *
 * Handle these with a `default` branch, or filter with `isKnownEvent`.
 */
export interface UnknownApiEvent {
  ev: string;
  [key: string]: unknown;
}

/** Any frame off the wire, known or not. Narrow with `isKnownEvent`. */
export type AnyApiEvent = ApiEvent | UnknownApiEvent;

export type ApiEventKind = ApiEvent["ev"];

export interface ClientFrame {
  v: number;
  id: number;
  [key: string]: unknown;
}

export type ServerFrame = { v: number; reply_to?: number } & UnknownApiEvent;

/** Every event tag the SDK knows about, for drift checks and routing. */
export const KNOWN_EVENT_KINDS = [
  "session_inspection",
  "output_cleanup",
  "execution",
  "primary_control_capabilities",
  "primary_input_receipt",
  "primary_input_detail",
  "primary_location",
  "primary_launch_capabilities",
  "primary_launch",
  "hello_ok",
  "ok",
  "error",
  "sessions",
  "attached",
  "startup_context_creation_failed",
  "history",
  "agents",
  "agent_changed",
  "agent_status",
  "workflow_prompt_rendered",
  "pong",
  "text_delta",
  "reasoning_delta",
  "reasoning_done",
  "tool_start",
  "tool_input_delta",
  "tool_exec",
  "tool_done",
  "token_usage",
  "turn_done",
  "background_progress",
  "message_accepted",
  "permission_request",
  "session_status",
  "model_info",
  "models",
  "runtime_info",
  "credential_updated",
  "file_content",
  "files",
  "text_matches",
  "file_status",
  "session_renamed",
] as const;

/** Every request tag the SDK can send. */
export const KNOWN_REQUEST_KINDS = [
  "session_inspection",
  "output_cleanup",
  "execution",
  "hello",
  "list_sessions",
  "archive_session",
  "restore_session",
  "set_retention_policy",
  "primary_control_probe",
  "primary_input",
  "primary_input_inspect",
  "primary_input_read",
  "primary_location",
  "primary_launch_probe",
  "primary_launch",
  "create_session",
  "attach_session",
  "detach_session",
  "send_message",
  "cancel",
  "soft_interrupt",
  "get_history",
  "list_agents",
  "set_agent",
  "inspect_agent",
  "render_workflow_prompt",
  "peek_session",
  "clear",
  "rewind",
  "permission_response",
  "list_models",
  "get_runtime_info",
  "set_api_key",
  "clear_api_key",
  "read_file",
  "find_files",
  "search_text",
  "file_status",
  "set_model",
  "set_reasoning_effort",
  "rename_session",
  "rewind_undo",
  "cancel_soft_interrupts",
  "ping",
] as const;

export function isKnownEvent(frame: AnyApiEvent): frame is ApiEvent {
  return (KNOWN_EVENT_KINDS as readonly string[]).includes(frame.ev);
}
