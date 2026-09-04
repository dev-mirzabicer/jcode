use super::*;

/// Serde default for boolean fields that should default to `true` when absent,
/// so older clients that omit the field keep their previous (unconditional)
/// behavior.
fn default_true() -> bool {
    true
}

/// Wire spec for a task-DAG node submitted by an agent (seed/expand/inject).
/// Mirrors `jcode_plan::dag::NodeSpec` but kept as an explicit wire type so the
/// protocol stays self-describing and serde-stable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskGraphNodeSpec {
    pub id: String,
    pub content: String,
    /// "explore" | "implement" | "verify" | "fix" | "synthesize". Defaults to
    /// "explore" when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub priority: u8,
}

fn is_zero_u8(value: &u8) -> bool {
    *value == 0
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_true(value: &bool) -> bool {
    *value
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LegacyContextCommand {
    #[default]
    Compact,
    SetCompactionMode,
}

/// Client request to server
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Request {
    /// Send a message to the agent
    #[serde(rename = "message")]
    Message {
        id: u64,
        content: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<(String, String)>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_reminder: Option<String>,
        /// Append the user message as context only. The daemon persists it and
        /// acknowledges it without starting a model turn.
        #[serde(default, skip_serializing_if = "is_false")]
        no_reply: bool,
        /// Observe captured Startup Context files before this turn. Older
        /// clients omit the field and retain ordinary real-user semantics.
        /// Synthetic continuations and provider retries set it to false.
        #[serde(default = "default_true", skip_serializing_if = "is_true")]
        observe_startup_context: bool,
    },

    /// Cancel current generation
    #[serde(rename = "cancel")]
    Cancel { id: u64 },

    /// Move the currently executing tool to background
    #[serde(rename = "background_tool")]
    BackgroundTool { id: u64 },

    /// Soft interrupt: inject message at next safe point without cancelling
    #[serde(rename = "soft_interrupt")]
    SoftInterrupt {
        id: u64,
        content: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<(String, String)>,
        /// If true, can skip remaining tools at injection point C
        #[serde(default)]
        urgent: bool,
    },

    /// Cancel all pending soft interrupts (remove from server queue before injection)
    #[serde(rename = "cancel_soft_interrupts")]
    CancelSoftInterrupts { id: u64 },

    /// Clear conversation history
    #[serde(rename = "clear")]
    Clear { id: u64 },

    /// Select the active primary agent. Ordinary post-dispatch changes append a
    /// complete profile; `replace` explicitly replaces the true system prompt.
    #[serde(rename = "set_agent")]
    SetAgent {
        id: u64,
        agent: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        replace: bool,
    },

    /// List currently valid primary agents for the active project.
    #[serde(rename = "get_agent_catalog")]
    GetAgentCatalog { id: u64 },

    /// Inspect exact current profile state. Complete prompt and skill text are
    /// returned only on this explicit request, not in ordinary History.
    #[serde(rename = "get_agent_status")]
    GetAgentStatus {
        id: u64,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        include_instructions: bool,
    },

    /// Rewind conversation history to the given 1-based message index.
    #[serde(rename = "rewind")]
    Rewind { id: u64, message_index: usize },

    /// Undo the most recent rewind, if one is available.
    #[serde(rename = "rewind_undo")]
    RewindUndo { id: u64 },

    /// Health check
    #[serde(rename = "ping")]
    Ping { id: u64 },

    /// Get current state (debug)
    #[serde(rename = "state")]
    GetState { id: u64 },

    /// Execute a debug command (debug socket only)
    #[serde(rename = "debug_command")]
    DebugCommand {
        id: u64,
        command: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },

    /// Execute a client debug command (forwarded to TUI)
    #[serde(rename = "client_debug_command")]
    ClientDebugCommand { id: u64, command: String },

    /// Response from TUI for client debug command
    #[serde(rename = "client_debug_response")]
    ClientDebugResponse { id: u64, output: String },

    /// Subscribe to events (for TUI clients)
    #[serde(rename = "subscribe")]
    Subscribe {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selfdev: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_session_id: Option<String>,
        /// Optional initial primary agent. Ignored when attaching to an existing
        /// session, whose exact stored activation remains authoritative.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        startup_context_caller: Option<StartupContextPrimaryCaller>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_instance_id: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        client_has_local_history: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        allow_session_takeover: bool,
        /// Terminal-identifying env vars (tmux/zellij/kitty/DISPLAY/...) captured
        /// from the connecting client so the server can route spawn/focus hooks
        /// to the client's terminal instead of its own stale startup env (#405).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        terminal_env: Vec<(String, String)>,
    },

    /// Get full conversation history (for TUI sync on connect)
    #[serde(rename = "get_history")]
    GetHistory { id: u64 },

    /// Get only provider/model metadata and available models.
    #[serde(rename = "get_model_catalog")]
    GetModelCatalog { id: u64 },

    /// Get a bounded view of compacted historical messages for lazy transcript expansion.
    #[serde(rename = "get_compacted_history")]
    GetCompactedHistory {
        id: u64,
        /// Number of leading compacted messages the client wants rendered before the live tail.
        visible_messages: usize,
    },

    /// Get one bounded page of authoritative Startup Context receipt state.
    #[serde(rename = "get_startup_context_status")]
    GetStartupContextStatus {
        id: u64,
        #[serde(default)]
        file_page_start: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_page_size: Option<usize>,
        #[serde(default)]
        issue_page_start: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        issue_page_size: Option<usize>,
    },

    /// Acquire the one editable Startup Context lease for the active project.
    #[serde(rename = "open_startup_context_editor")]
    OpenStartupContextEditor { id: u64 },

    /// Renew a live Startup Context editor lease.
    #[serde(rename = "renew_startup_context_editor_lease")]
    RenewStartupContextEditorLease {
        id: u64,
        lease_id: String,
        project_key_digest: String,
        expected_plan_revision: u64,
    },

    /// Explicitly close a live Startup Context editor lease.
    #[serde(rename = "close_startup_context_editor")]
    CloseStartupContextEditor {
        id: u64,
        lease_id: String,
        project_key_digest: String,
    },

    /// List one bounded project-rooted directory page for the Startup Context editor.
    #[serde(rename = "list_startup_context_directory")]
    ListStartupContextDirectory {
        id: u64,
        lease_id: String,
        project_key_digest: String,
        expected_plan_revision: u64,
        directory: String,
        #[serde(default)]
        page_start: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        page_size: Option<usize>,
    },

    /// Start one bounded, cancellable project-file-name search.
    #[serde(rename = "search_startup_context_files")]
    SearchStartupContextFiles {
        id: u64,
        lease_id: String,
        project_key_digest: String,
        expected_plan_revision: u64,
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_results: Option<usize>,
    },

    /// Cancel an active Startup Context search from this client connection.
    #[serde(rename = "cancel_startup_context_search")]
    CancelStartupContextSearch { id: u64, search_request_id: u64 },

    /// Preview the complete current UTF-8 file through one bounded character chunk.
    #[serde(rename = "preview_startup_context_file")]
    PreviewStartupContextFile {
        id: u64,
        lease_id: String,
        project_key_digest: String,
        expected_plan_revision: u64,
        path: String,
        #[serde(default)]
        start_char: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_chars: Option<usize>,
    },

    /// Retrieve one bounded exact chunk from a receipt-owned captured file message.
    #[serde(rename = "get_startup_context_file_detail")]
    GetStartupContextFileDetail {
        id: u64,
        batch_id: String,
        spec_id: String,
        message_id: String,
        expected_sha256: String,
        #[serde(default)]
        start_char: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_chars: Option<usize>,
    },

    /// Normalize and validate one complete ordered Startup Context selection.
    #[serde(rename = "preview_startup_context_selection")]
    PreviewStartupContextSelection {
        id: u64,
        lease_id: String,
        project_key_digest: String,
        expected_plan_revision: u64,
        selection: Vec<StartupContextSelectionInput>,
    },

    /// Atomically apply one ordered selection to the session and optionally its project default.
    #[serde(rename = "apply_startup_context_selection")]
    ApplyStartupContextSelection {
        id: u64,
        operation_id: String,
        lease_id: String,
        project_key_digest: String,
        expected_plan_revision: u64,
        selection: Vec<StartupContextSelectionInput>,
        #[serde(default)]
        save_project_default: bool,
    },

    /// Cancel a queued Startup Context apply before it begins committing targets.
    #[serde(rename = "cancel_startup_context_apply")]
    CancelStartupContextApply {
        id: u64,
        operation_id: String,
        lease_id: String,
        project_key_digest: String,
        expected_plan_revision: u64,
    },

    /// Inspect one durable Startup Context apply operation.
    #[serde(rename = "get_startup_context_apply_status")]
    GetStartupContextApplyStatus { id: u64, operation_id: String },

    /// Get one bounded page of the authoritative context-editor snapshot.
    #[serde(rename = "get_context_editor_snapshot")]
    GetContextEditorSnapshot {
        id: u64,
        #[serde(default)]
        page_start: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        page_size: Option<usize>,
    },

    /// Lazily retrieve one bounded, image-safe content-block detail chunk.
    #[serde(rename = "get_context_message_detail")]
    GetContextMessageDetail {
        id: u64,
        expected_context_revision: u64,
        expected_transcript_digest: u64,
        message_id: String,
        block_ordinal: usize,
        #[serde(default)]
        start_char: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_chars: Option<usize>,
    },

    /// Resolve and explain structurally closed summary ranges without invoking a curator.
    #[serde(rename = "preview_context_ranges")]
    PreviewContextRanges {
        id: u64,
        expected_context_revision: u64,
        expected_transcript_digest: u64,
        ranges: Vec<ContextMessageRangeSelection>,
    },

    /// Build and validate the exact isolated curator-call plan without invoking a model.
    #[serde(rename = "preview_context_curator_plan")]
    PreviewContextCuratorPlan {
        id: u64,
        expected_context_revision: u64,
        expected_transcript_digest: u64,
        request: ContextDraftRequest,
    },

    /// Persist only provider/route/model/effort as the global curator default.
    #[serde(rename = "save_context_curator_default")]
    SaveContextCuratorDefault {
        id: u64,
        selection: ContextCuratorSelection,
    },

    /// Capture and prepare one atomic context transaction draft.
    #[serde(rename = "prepare_context_draft")]
    PrepareContextDraft {
        id: u64,
        request: ContextDraftRequest,
    },

    /// Cancel a preparing or ready draft.
    #[serde(rename = "cancel_context_draft")]
    CancelContextDraft { id: u64, draft_id: String },

    /// Reconnect to a retained draft by ID and retrieve its current status.
    #[serde(rename = "get_context_draft_status")]
    GetContextDraftStatus { id: u64, draft_id: String },

    /// Recalculate a ready draft for an exact distillation subset without rerunning the curator.
    #[serde(rename = "preview_context_draft_selection")]
    PreviewContextDraftSelection {
        id: u64,
        draft_id: String,
        #[serde(default)]
        selected_distillation_ids: Vec<String>,
    },

    /// Atomically apply a ready draft. `None` selects curator defaults; `Some`
    /// applies exactly the supplied validated subset.
    #[serde(rename = "apply_context_draft")]
    ApplyContextDraft {
        id: u64,
        draft_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected_distillation_ids: Option<Vec<String>>,
    },

    /// List bounded context transaction provenance, newest first.
    #[serde(rename = "list_context_transactions")]
    ListContextTransactions {
        id: u64,
        #[serde(default)]
        offset: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
    },

    /// Retrieve one complete persisted transaction for provenance inspection.
    #[serde(rename = "get_context_transaction_detail")]
    GetContextTransactionDetail {
        id: u64,
        expected_context_revision: u64,
        transaction_id: String,
    },

    /// Revert one active context transaction.
    #[serde(rename = "revert_context_transaction")]
    RevertContextTransaction { id: u64, transaction_id: String },

    /// Reapply one inactive context transaction after current target validation.
    #[serde(rename = "reapply_context_transaction")]
    ReapplyContextTransaction { id: u64, transaction_id: String },

    /// Persist the explicit unattended emergency policy. Step 11 consumes the
    /// authorization; Step 8 exposes and persists it without implicit enablement.
    #[serde(rename = "set_context_emergency_policy")]
    SetContextEmergencyPolicy {
        id: u64,
        policy: jcode_session_types::StoredContextEmergencyPolicy,
    },

    /// Trigger server hot reload (build new version, restart)
    #[serde(rename = "reload")]
    Reload {
        id: u64,
        /// When `true` (the default for backward compatibility), the server
        /// reloads unconditionally. When `false`, the server only reloads if it
        /// detects a strictly-newer reload candidate binary, so callers like
        /// `jcode server reload` can request a graceful upgrade without risking
        /// a downgrade (e.g. a newer self-dev daemon next to an older release).
        #[serde(default = "default_true")]
        force: bool,
    },

    /// Resume a specific session by ID
    #[serde(rename = "resume_session")]
    ResumeSession {
        id: u64,
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_instance_id: Option<String>,
        #[serde(default)]
        client_has_local_history: bool,
        #[serde(default)]
        allow_session_takeover: bool,
    },

    /// Resume/continue every live session that was interrupted and would
    /// auto-continue on a reload (e.g. crashed/errored mid-turn). This is the
    /// on-demand equivalent of the automatic post-reload recovery sweep.
    #[serde(rename = "resume_all_sessions")]
    ResumeAllSessions { id: u64 },

    /// Deliver a scheduled task to a currently live session.
    #[serde(rename = "notify_session")]
    NotifySession {
        id: u64,
        session_id: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unattended_context: Option<jcode_session_types::StoredUnattendedContextAuthorization>,
    },

    /// Inject externally transcribed text into a live TUI session.
    #[serde(rename = "transcript")]
    Transcript {
        id: u64,
        text: String,
        #[serde(default)]
        mode: TranscriptMode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },

    /// Execute a shell command from `!cmd` in the active remote session.
    #[serde(rename = "input_shell")]
    InputShell { id: u64, command: String },

    /// Cycle the active model (direction: 1 for next, -1 for previous)
    #[serde(rename = "cycle_model")]
    CycleModel {
        id: u64,
        #[serde(default = "default_model_direction")]
        direction: i8,
    },

    #[serde(rename = "refresh_models")]
    RefreshModels { id: u64 },

    /// Set the active model by name.
    ///
    /// A legacy/desktop compatibility shape (`{"type":"set_route","model":...}`)
    /// is also accepted, but it is normalized into this variant inside
    /// [`crate::decode_request`] rather than via a serde `alias`. A serde alias
    /// would make this variant *also* answer to the `set_route` tag, and serde's
    /// internally-tagged enums pick the first matching variant by tag (not by
    /// fields), so it would shadow the structured [`Request::SetRoute`] variant
    /// below and make every structured route switch fail with
    /// `missing field \`model\``.
    #[serde(rename = "set_model")]
    SetModel { id: u64, model: String },

    /// Set the active model by structured route identity.
    #[serde(rename = "set_route")]
    SetRoute {
        id: u64,
        selection: jcode_provider_core::RouteSelection,
    },

    /// Set or clear the session-scoped subagent model preference.
    #[serde(rename = "set_subagent_model")]
    SetSubagentModel {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },

    /// Launch a subagent immediately in the active session.
    #[serde(rename = "run_subagent")]
    RunSubagent {
        id: u64,
        prompt: String,
        subagent_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },

    /// Set reasoning effort for providers that expose it (OpenAI: none|minimal|low|medium|high|xhigh|max; Anthropic: none|low|medium|high|xhigh|max; DeepSeek: none|low|medium|high|max)
    #[serde(rename = "set_reasoning_effort")]
    SetReasoningEffort {
        id: u64,
        effort: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_session_id: Option<String>,
    },

    /// Set service tier for OpenAI models (priority|fast|flex|off)
    #[serde(rename = "set_service_tier")]
    SetServiceTier { id: u64, service_tier: String },

    /// Set connection transport for OpenAI models (auto|https|websocket)
    #[serde(rename = "set_transport")]
    SetTransport { id: u64, transport: String },

    /// Set Copilot premium request conservation mode (0=normal, 1=one-per-session, 2=zero)
    #[serde(rename = "set_premium_mode")]
    SetPremiumMode { id: u64, mode: u8 },

    /// Toggle a runtime feature for this session
    #[serde(rename = "set_feature")]
    SetFeature {
        id: u64,
        feature: FeatureToggle,
        enabled: bool,
    },

    /// Compatibility-only envelope produced by [`crate::decode_request`] for
    /// obsolete clients. It has no historical wire tag and cannot execute a
    /// context mutation.
    #[serde(rename = "__legacy_context_command")]
    LegacyContextCommand {
        id: u64,
        #[serde(skip)]
        command: LegacyContextCommand,
    },

    /// Set or clear the active session's custom display title.
    #[serde(rename = "rename_session")]
    RenameSession {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },

    /// Split the current session — clone conversation into a new session
    #[serde(rename = "split")]
    Split { id: u64 },

    /// Transfer the current session into a compacted handoff session
    #[serde(rename = "transfer")]
    Transfer { id: u64 },

    /// Trigger immediate memory extraction for the current session
    #[serde(rename = "trigger_memory_extraction")]
    TriggerMemoryExtraction { id: u64 },

    /// Notify server that auth credentials changed (e.g., after login)
    #[serde(rename = "notify_auth_changed")]
    NotifyAuthChanged {
        id: u64,
        /// Optional runtime provider identity whose credentials changed. Older
        /// clients omit this and get the legacy generic refresh behavior.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        /// Typed auth lifecycle event for new clients. The legacy `provider`
        /// string is retained for old clients, while this payload gives the
        /// server enough context to activate the intended runtime/catalog
        /// profile deterministically.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth: Option<AuthChanged>,
        /// First-run onboarding may ask the server to choose the strongest
        /// available route across all authenticated providers. Normal re-auth,
        /// account switching, and older clients leave this false.
        #[serde(default, skip_serializing_if = "is_false")]
        prefer_strongest: bool,
    },

    /// Switch active Anthropic account label on the server session.
    /// This keeps account overrides and provider credential caches in sync.
    #[serde(rename = "switch_anthropic_account")]
    SwitchAnthropicAccount { id: u64, label: String },

    /// Switch active OpenAI account label on the server session.
    /// This keeps account overrides and provider credential caches in sync.
    #[serde(rename = "switch_openai_account")]
    SwitchOpenAiAccount { id: u64, label: String },

    /// Send stdin input to a running command that requested it
    #[serde(rename = "stdin_response")]
    StdinResponse {
        id: u64,
        /// Matches the request_id from StdinRequest
        request_id: String,
        /// The user's input (line of text)
        input: String,
    },

    // === Agent-to-agent communication ===
    /// Register as an external agent
    #[serde(rename = "agent_register")]
    AgentRegister {
        id: u64,
        agent_name: String,
        capabilities: Vec<String>,
    },

    /// Send a task to jcode agent
    #[serde(rename = "agent_task")]
    AgentTask {
        id: u64,
        from_agent: String,
        task: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        context: Option<serde_json::Value>,
        /// Whether to wait for completion or return immediately
        #[serde(default)]
        async_: bool,
    },

    /// Query jcode agent's capabilities
    #[serde(rename = "agent_capabilities")]
    AgentCapabilities { id: u64 },

    /// Get conversation context (for handoff between agents)
    #[serde(rename = "agent_context")]
    AgentContext { id: u64 },

    // === Agent communication ===
    /// Share context with other agents
    #[serde(rename = "comm_share")]
    CommShare {
        id: u64,
        session_id: String,
        key: String,
        value: String,
        #[serde(default)]
        append: bool,
    },

    /// Read shared context from other agents
    #[serde(rename = "comm_read")]
    CommRead {
        id: u64,
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },

    /// Send a message to other agents
    #[serde(rename = "comm_message")]
    CommMessage {
        id: u64,
        from_session: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to_session: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delivery: Option<CommDeliveryMode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wake: Option<bool>,
        /// Sender-provided one-line summary. Receiving UIs render long
        /// message bodies collapsed to this with an expand control.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tldr: Option<String>,
    },

    /// List agents and their activity
    #[serde(rename = "comm_list")]
    CommList { id: u64, session_id: String },

    /// List swarm channels and subscriber counts
    #[serde(rename = "comm_list_channels")]
    CommListChannels { id: u64, session_id: String },

    /// List members subscribed to a swarm channel
    #[serde(rename = "comm_channel_members")]
    CommChannelMembers {
        id: u64,
        session_id: String,
        channel: String,
    },

    /// Propose a swarm plan update
    #[serde(rename = "comm_propose_plan")]
    CommProposePlan {
        id: u64,
        session_id: String,
        items: Vec<PlanItem>,
    },

    /// Approve a plan proposal (coordinator only)
    #[serde(rename = "comm_approve_plan")]
    CommApprovePlan {
        id: u64,
        session_id: String,
        proposer_session: String,
    },

    /// Reject a plan proposal (coordinator only)
    #[serde(rename = "comm_reject_plan")]
    CommRejectPlan {
        id: u64,
        session_id: String,
        proposer_session: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },

    /// Seed the swarm task DAG in one call (the first agent's draft). Replaces or
    /// initializes the shared plan with a validated graph of nodes + edges.
    #[serde(rename = "comm_seed_graph")]
    CommSeedGraph {
        id: u64,
        session_id: String,
        /// "deep" (comprehensive, gated) or "light" (fan-out).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
        nodes: Vec<TaskGraphNodeSpec>,
    },

    /// Decompose a node the caller owns into a child sub-DAG (composite path). In
    /// deep mode a critique/verify gate is auto-inserted.
    #[serde(rename = "comm_expand_node")]
    CommExpandNode {
        id: u64,
        session_id: String,
        node_id: String,
        children: Vec<TaskGraphNodeSpec>,
    },

    /// Complete a node the caller owns with a typed handoff artifact. In deep mode
    /// the artifact is validated for thinness.
    #[serde(rename = "comm_complete_node")]
    CommCompleteNode {
        id: u64,
        session_id: String,
        node_id: String,
        /// Handoff artifact as a JSON object string.
        artifact_json: String,
    },

    /// Inject gap/fix nodes from a gate that found a problem, re-blocking the gate
    /// (and its composite parent) until the new nodes drain.
    #[serde(rename = "comm_inject_gap")]
    CommInjectGap {
        id: u64,
        session_id: String,
        gate_id: String,
        nodes: Vec<TaskGraphNodeSpec>,
    },

    /// Spawn a new agent session (coordinator only)
    #[serde(rename = "comm_spawn")]
    CommSpawn {
        id: u64,
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initial_message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_nonce: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spawn_mode: Option<String>,
        /// Optional per-spawn model override. Takes precedence over
        /// `agents.swarm_model` config. Supports explicit auth-route prefixes
        /// (e.g. `openai-api:gpt-5.5`) and the `inherit`/`coordinator`
        /// sentinels to force coordinator inheritance past a config pin.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Optional reasoning effort for the spawned agent (e.g. `none`,
        /// `low`, `medium`, `high`, `xhigh`, `max`). Unset = provider default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
        /// Optional short human-readable label for the spawned agent shown in
        /// swarm UI (gallery chips, member lists). Overrides the task label
        /// otherwise derived from the first line of `initial_message`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },

    /// List models/routes available for spawning swarm agents
    #[serde(rename = "comm_list_models")]
    CommListModels { id: u64, session_id: String },

    /// Stop/destroy an agent session (coordinator only)
    #[serde(rename = "comm_stop")]
    CommStop {
        id: u64,
        session_id: String,
        target_session: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        force: Option<bool>,
    },

    /// Assign a role to an agent (coordinator only)
    #[serde(rename = "comm_assign_role")]
    CommAssignRole {
        id: u64,
        session_id: String,
        target_session: String,
        role: String,
    },

    /// Get a summary of an agent's recent tool calls
    #[serde(rename = "comm_summary")]
    CommSummary {
        id: u64,
        session_id: String,
        target_session: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
    },

    /// Get a lightweight status snapshot for an agent, even while it is busy
    #[serde(rename = "comm_status")]
    CommStatus {
        id: u64,
        session_id: String,
        target_session: String,
    },

    /// Submit a structured swarm completion/progress report for this session
    #[serde(rename = "comm_report")]
    CommReport {
        id: u64,
        session_id: String,
        /// Completion status to record for this member. Defaults to ready.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        /// Main report body.
        message: String,
        /// Optional validation/testing summary.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        validation: Option<String>,
        /// Optional blockers/follow-up summary.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        follow_up: Option<String>,
        /// Reporter-provided one-line summary. Receiving UIs render long
        /// report bodies collapsed to this with an expand control.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tldr: Option<String>,
    },

    /// Read another agent's full conversation context
    #[serde(rename = "comm_read_context")]
    CommReadContext {
        id: u64,
        session_id: String,
        target_session: String,
    },

    /// Attach/resync this session with the swarm plan
    #[serde(rename = "comm_resync_plan")]
    CommResyncPlan { id: u64, session_id: String },

    /// Get a lightweight summary of the current swarm plan graph
    #[serde(rename = "comm_plan_status")]
    CommPlanStatus { id: u64, session_id: String },

    /// Assign a task from the plan to a specific agent (coordinator only)
    #[serde(rename = "comm_assign_task")]
    CommAssignTask {
        id: u64,
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_session: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },

    /// Assign the next runnable unassigned task from the plan (coordinator only)
    #[serde(rename = "comm_assign_next")]
    CommAssignNext {
        id: u64,
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_session: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefer_spawn: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spawn_if_needed: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        /// Optional model override for workers spawned by this assignment
        /// (same semantics as CommSpawn::model).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Optional reasoning effort for workers spawned by this assignment
        /// (same semantics as CommSpawn::effort).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
    },

    /// Control an existing assigned task lifecycle (coordinator only)
    #[serde(rename = "comm_task_control")]
    CommTaskControl {
        id: u64,
        session_id: String,
        action: String,
        task_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_session: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },

    /// Subscribe to a named channel in the swarm
    #[serde(rename = "comm_subscribe_channel")]
    CommSubscribeChannel {
        id: u64,
        session_id: String,
        channel: String,
    },

    /// Unsubscribe from a named channel in the swarm
    #[serde(rename = "comm_unsubscribe_channel")]
    CommUnsubscribeChannel {
        id: u64,
        session_id: String,
        channel: String,
    },

    /// Wait until specified (or all) swarm members reach a target status
    #[serde(rename = "comm_await_members")]
    CommAwaitMembers {
        id: u64,
        session_id: String,
        /// Statuses that count as "done" (e.g. ["completed", "stopped"])
        target_status: Vec<String>,
        /// Specific session IDs to watch. If empty, watches all non-self members.
        #[serde(default)]
        session_ids: Vec<String>,
        /// Whether to wait for all matching members or wake when any member matches.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
        /// Timeout in seconds (default 3600 = 1 hour)
        #[serde(default)]
        timeout_secs: Option<u64>,
        /// Run the wait as a detached background watcher instead of blocking the
        /// requesting turn. Defaults to true so the agent stays responsive.
        #[serde(default = "default_true")]
        background: bool,
        /// When backgrounded, surface a notification card on completion.
        #[serde(default = "default_true")]
        notify: bool,
        /// When backgrounded, wake an idle requesting agent with the result (or
        /// soft-interrupt it if busy). Defaults to true.
        #[serde(default = "default_true")]
        wake: bool,
    },
}

/// Server event sent to client
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
    /// Acknowledgment of request
    #[serde(rename = "ack")]
    Ack { id: u64 },

    /// Streaming text delta
    #[serde(rename = "text_delta")]
    TextDelta { text: String },

    /// Streaming reasoning/thinking delta (raw, unformatted model text).
    ///
    /// Unlike [`ServerEvent::TextDelta`], this carries the model's reasoning as
    /// raw text deltas so the client can render the in-progress line live
    /// (token-by-token) rather than waiting for a whole line to complete. The
    /// client is responsible for the dim+italic styling. Clients that predate
    /// this event simply ignore it (reasoning is still persisted as a
    /// history-only trace and shown when the message commits).
    #[serde(rename = "reasoning_delta")]
    ReasoningDelta { text: String },

    /// Reasoning/thinking finished for the current step. Lets the client close
    /// its live reasoning region (flush the partial line, add separators) before
    /// normal output or a tool call begins.
    #[serde(rename = "reasoning_done")]
    ReasoningDone {
        /// Wall-clock reasoning duration in seconds, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_secs: Option<f64>,
    },

    /// Replace the current turn's streamed text content
    /// Used when text-wrapped tool calls are recovered: the garbled text
    /// shown during streaming is replaced with the clean prefix text.
    #[serde(rename = "text_replace")]
    TextReplace { text: String },

    /// Tool call started
    #[serde(rename = "tool_start")]
    ToolStart { id: String, name: String },

    /// Tool input delta (streaming JSON)
    #[serde(rename = "tool_input")]
    ToolInput { delta: String },

    /// Tool call ended, now executing
    #[serde(rename = "tool_exec")]
    ToolExec { id: String, name: String },

    /// Tool execution completed
    #[serde(rename = "tool_done")]
    ToolDone {
        id: String,
        name: String,
        output: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    /// Rendered images produced during the live turn, including image-bearing
    /// tool results and provider-native image generation. Lets remote clients
    /// render them inline immediately instead of waiting for History reload.
    #[serde(rename = "side_pane_images")]
    SidePaneImages {
        session_id: String,
        images: Vec<jcode_session_types::RenderedImage>,
    },

    /// Image generated by a provider-native image generation tool.
    #[serde(rename = "generated_image")]
    GeneratedImage {
        id: String,
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata_path: Option<String>,
        output_format: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        revised_prompt: Option<String>,
    },

    /// Batch tool progress update, including currently-running subcalls
    #[serde(rename = "batch_progress")]
    BatchProgress { progress: BatchProgress },

    /// Token usage update
    #[serde(rename = "tokens")]
    TokenUsage {
        input: u64,
        output: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_read_input: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_creation_input: Option<u64>,
    },

    /// Prompt-shape signature for the API request that will later report token
    /// usage. Remote clients use this to diagnose KV-cache misses.
    #[serde(rename = "kv_cache_request")]
    KvCacheRequest {
        system_static_hash: u64,
        tools_hash: u64,
        messages_hash: u64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        message_hashes: Vec<u64>,
        message_count: usize,
        tool_count: usize,
        #[serde(default)]
        system_static_chars: usize,
        #[serde(default)]
        tools_json_chars: usize,
        #[serde(default)]
        messages_json_chars: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ephemeral_hash: Option<u64>,
        #[serde(default)]
        ephemeral_chars: usize,
        #[serde(default)]
        ephemeral_message_count: usize,
    },

    /// Active transport/connection type for the current stream
    #[serde(rename = "connection_type")]
    ConnectionType { connection: String },

    /// Connection phase update (authenticating, connecting, waiting, etc.)
    #[serde(rename = "connection_phase")]
    ConnectionPhase { phase: String },

    /// Provider-supplied human-readable transport detail for the current stream.
    #[serde(rename = "status_detail")]
    StatusDetail { detail: String },

    /// Provider has finished the visible assistant message, but the turn may still be
    /// finalizing bookkeeping such as session IDs or completion trailers.
    ///
    /// `stop_reason` carries the provider's own reason when it supplied one
    /// (e.g. Anthropic `end_turn`, `tool_use`, `max_tokens`). It must be
    /// forwarded rather than dropped: `max_tokens` is the only signal that a
    /// turn was truncated by the output budget, and headless consumers
    /// (`run --ndjson`) have no other way to detect it.
    #[serde(rename = "message_end")]
    MessageEnd {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop_reason: Option<String>,
    },

    /// A transient transport fault interrupted the provider stream mid-response
    /// and the provider is retrying the request from the top. The client must
    /// discard all partial output from the current attempt (streamed text,
    /// reasoning, in-progress tool calls) so the replayed response renders
    /// cleanly instead of duplicating.
    #[serde(rename = "retry_rollback")]
    RetryRollback { attempt: u32, max: u32 },

    /// Upstream provider info (e.g., which provider OpenRouter routed to)
    #[serde(rename = "upstream_provider")]
    UpstreamProvider { provider: String },

    /// Swarm status update (subagent/session lifecycle info)
    #[serde(rename = "swarm_status")]
    SwarmStatus { members: Vec<SwarmMemberStatus> },

    /// Full swarm plan snapshot for synchronization and UI rendering.
    #[serde(rename = "swarm_plan")]
    SwarmPlan {
        swarm_id: String,
        version: u64,
        items: Vec<PlanItem>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        participants: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<PlanGraphStatus>,
    },

    /// Plan proposal payload delivered to the coordinator.
    #[serde(rename = "swarm_plan_proposal")]
    SwarmPlanProposal {
        swarm_id: String,
        proposer_session: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        proposer_name: Option<String>,
        items: Vec<PlanItem>,
        summary: String,
        proposal_key: String,
    },

    /// Soft interrupt message was injected at a safe point
    #[serde(rename = "soft_interrupt_injected")]
    SoftInterruptInjected {
        /// The injected message content
        content: String,
        /// Optional display role override for the injected content (e.g. "system")
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_role: Option<String>,
        /// Which injection point: "A" (after stream), "B" (no tools),
        /// "C" (between tools), "D" (after all tools)
        point: String,
        /// Number of tools skipped (only for urgent interrupt at point C)
        #[serde(skip_serializing_if = "Option::is_none")]
        tools_skipped: Option<usize>,
    },

    /// Current turn was interrupted by explicit user cancel.
    ///
    /// This is rendered as a system/status notice (not assistant content),
    /// so it does not blend into streaming model output.
    #[serde(rename = "interrupted")]
    Interrupted,

    /// The provider ended the turn without any visible assistant output,
    /// typically a model-side guardrail/refusal stop (e.g. Anthropic
    /// `stop_reason: "refusal"`), or a reasoning-only response with no final
    /// text. Rendered as a system notice so the user learns why no response
    /// arrived instead of the turn ending silently.
    #[serde(rename = "provider_guardrail")]
    ProviderGuardrail {
        /// Raw provider stop reason, when known (e.g. "refusal").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop_reason: Option<String>,
        /// Human-readable explanation for display.
        message: String,
    },

    /// Relevant memory was injected into the conversation
    #[serde(rename = "memory_injected")]
    MemoryInjected {
        /// Number of memories injected
        count: usize,
        /// Exact memory content that was injected
        #[serde(default)]
        prompt: String,
        /// Display-only version of the injected memory content.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_prompt: Option<String>,
        /// Character length of injected content
        #[serde(default)]
        prompt_chars: usize,
        /// Age of the precomputed memory payload at injection time
        #[serde(default)]
        computed_age_ms: u64,
    },

    /// Memory activity state update for remote clients.
    #[serde(rename = "memory_activity")]
    MemoryActivity { activity: MemoryActivitySnapshot },

    /// Message/turn completed
    #[serde(rename = "done")]
    Done { id: u64 },

    /// A context-only user message was appended and persisted. This is distinct
    /// from `done`: no model turn was started and no turn boundary should be
    /// emitted to API clients.
    #[serde(rename = "context_message_added")]
    ContextMessageAdded { id: u64 },

    /// Error occurred
    #[serde(rename = "error")]
    Error {
        id: u64,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after_secs: Option<u64>,
    },

    /// Pong response
    #[serde(rename = "pong")]
    Pong { id: u64 },

    /// Current state (debug)
    #[serde(rename = "state")]
    State {
        id: u64,
        session_id: String,
        message_count: usize,
        is_processing: bool,
    },

    /// Response for debug command
    #[serde(rename = "debug_response")]
    DebugResponse { id: u64, ok: bool, output: String },

    /// MCP status update (sent after background MCP connections complete)
    #[serde(rename = "mcp_status")]
    McpStatus {
        /// Server names with tool counts in "name:count" format
        servers: Vec<String>,
    },

    /// Client debug command forwarded from debug socket to TUI
    #[serde(rename = "client_debug_request")]
    ClientDebugRequest { id: u64, command: String },

    /// Session ID assigned
    #[serde(rename = "session")]
    SessionId { session_id: String },

    /// Current primary agent identity for the session.
    #[serde(rename = "agent_selected")]
    AgentSelected {
        id: u64,
        agent_id: String,
        display_name: String,
        scope: String,
        #[serde(default)]
        change: crate::AgentProfileChangeKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_content: Option<String>,
    },

    #[serde(rename = "agent_catalog")]
    AgentCatalog {
        id: u64,
        agents: Vec<crate::AgentProfileSummary>,
    },

    #[serde(rename = "agent_status")]
    AgentStatus {
        id: u64,
        agent_id: String,
        display_name: String,
        scope: String,
        #[serde(default)]
        first_provider_dispatched: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_transition_message_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_skill: Option<String>,
    },

    /// Server requests that this client/session close itself.
    #[serde(rename = "session_close_requested")]
    SessionCloseRequested { reason: String },

    /// Session display title changed.
    #[serde(rename = "session_renamed")]
    SessionRenamed {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        display_title: String,
    },

    /// Full conversation history (response to GetHistory)
    #[serde(rename = "history")]
    History {
        id: u64,
        session_id: String,
        messages: Vec<HistoryMessage>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<jcode_session_types::RenderedImage>,
        /// Provider name (e.g. "anthropic", "openai")
        #[serde(skip_serializing_if = "Option::is_none")]
        provider_name: Option<String>,
        /// Model name (e.g. "claude-sonnet-4-20250514")
        #[serde(skip_serializing_if = "Option::is_none")]
        provider_model: Option<String>,
        /// Available models for this provider
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        available_models: Vec<String>,
        /// Route metadata for available models
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        available_model_routes: Vec<jcode_provider_core::ModelRoute>,
        /// Connected MCP server names
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mcp_servers: Vec<String>,
        /// Available skill names
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        skills: Vec<String>,
        /// Total session token usage (input, output)
        #[serde(skip_serializing_if = "Option::is_none")]
        total_tokens: Option<(u64, u64)>,
        /// Detailed persisted token usage totals for diagnostics and cache stats.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token_usage_totals: Option<TokenUsageTotals>,
        /// All session IDs on the server
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        all_sessions: Vec<String>,
        /// Number of connected clients
        #[serde(skip_serializing_if = "Option::is_none")]
        client_count: Option<usize>,
        /// Whether this session is in canary/self-dev mode
        #[serde(skip_serializing_if = "Option::is_none")]
        is_canary: Option<bool>,
        /// Server binary version string (e.g. "v0.1.123 (abc1234)")
        #[serde(skip_serializing_if = "Option::is_none")]
        server_version: Option<String>,
        /// Server name for multi-server support (e.g. "blazing")
        #[serde(skip_serializing_if = "Option::is_none")]
        server_name: Option<String>,
        /// Server icon for display (e.g. "🔥")
        #[serde(skip_serializing_if = "Option::is_none")]
        server_icon: Option<String>,
        /// Whether a newer server binary is available on disk
        #[serde(skip_serializing_if = "Option::is_none")]
        server_has_update: Option<bool>,
        /// Whether the session was interrupted mid-generation (crashed/disconnected while processing)
        #[serde(skip_serializing_if = "Option::is_none")]
        was_interrupted: Option<bool>,
        /// Server-owned reload recovery directive for this session, if a reconnect should continue automatically.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reload_recovery: Option<ReloadRecoverySnapshot>,
        /// Last observed actual connection type for this session (e.g. websocket, https/sse)
        #[serde(skip_serializing_if = "Option::is_none")]
        connection_type: Option<String>,
        /// Last observed provider-supplied status detail for this session.
        #[serde(skip_serializing_if = "Option::is_none")]
        status_detail: Option<String>,
        /// Upstream provider (e.g., which provider OpenRouter routed to, or calculated preference)
        #[serde(skip_serializing_if = "Option::is_none")]
        upstream_provider: Option<String>,
        /// Server-resolved billing credential for this session: `Oauth`
        /// (subscription) vs `ApiKey` (cost-based), or `None` when the active
        /// provider has no OAuth-vs-API-key distinction. Lets remote clients
        /// render usage/billing without re-deriving it from the provider name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolved_credential: Option<jcode_provider_core::ResolvedCredential>,
        /// Reasoning effort for providers that expose it
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<String>,
        /// Service tier override for OpenAI models
        #[serde(skip_serializing_if = "Option::is_none")]
        service_tier: Option<String>,
        /// Session-scoped preferred model for subagents.
        #[serde(skip_serializing_if = "Option::is_none")]
        subagent_model: Option<String>,
        /// Session-scoped automatic review toggle.
        #[serde(skip_serializing_if = "Option::is_none")]
        autoreview_enabled: Option<bool>,
        /// Session-scoped automatic judge toggle.
        #[serde(skip_serializing_if = "Option::is_none")]
        autojudge_enabled: Option<bool>,
        /// Persisted provider-context projection revision. New servers report
        /// this instead of an active automatic-compaction mode.
        #[serde(default)]
        context_revision: u64,
        /// Current live processing state for this session, if known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        activity: Option<SessionActivitySnapshot>,
        /// Session-scoped side panel pages and active focus state
        #[serde(default, skip_serializing_if = "snapshot_is_empty")]
        side_panel: SidePanelSnapshot,
        /// Bounded server-owned Startup Context status. Absence means the server
        /// predates the capability, not that the active project has an empty plan.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        startup_context: Option<Box<StartupContextCompactStatus>>,
    },

    #[serde(rename = "startup_context_status")]
    StartupContextStatus {
        id: u64,
        snapshot: StartupContextStatusSnapshot,
        /// Present only when this status response is the prompt-safe recovery
        /// signal for a blocked user dispatch. The additive field preserves
        /// compatibility with clients that already understand status events.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action_required: Option<StartupContextActionRequired>,
    },

    #[serde(rename = "startup_context_editor_opened")]
    StartupContextEditorOpened {
        id: u64,
        editor: StartupContextEditorSnapshot,
    },

    #[serde(rename = "startup_context_editor_busy")]
    StartupContextEditorBusy {
        id: u64,
        project: StartupContextProjectSnapshot,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owner: Option<StartupContextLeaseOwnerSnapshot>,
    },

    #[serde(rename = "startup_context_editor_lease_renewed")]
    StartupContextEditorLeaseRenewed {
        id: u64,
        lease: StartupContextLeaseSnapshot,
    },

    #[serde(rename = "startup_context_editor_closed")]
    StartupContextEditorClosed { id: u64, lease_id: String },

    #[serde(rename = "startup_context_directory_page")]
    StartupContextDirectoryPage {
        id: u64,
        page: StartupContextDirectoryPage,
    },

    #[serde(rename = "startup_context_search_results")]
    StartupContextSearchResults {
        id: u64,
        results: StartupContextSearchResults,
    },

    #[serde(rename = "startup_context_search_canceled")]
    StartupContextSearchCanceled {
        id: u64,
        search_request_id: u64,
        was_active: bool,
    },

    #[serde(rename = "startup_context_file_preview")]
    StartupContextFilePreview {
        id: u64,
        preview: StartupContextFilePreview,
    },

    #[serde(rename = "startup_context_file_detail")]
    StartupContextFileDetail {
        id: u64,
        detail: StartupContextFileDetail,
    },

    #[serde(rename = "startup_context_selection_preview")]
    StartupContextSelectionPreview {
        id: u64,
        preview: StartupContextSelectionPreview,
    },

    #[serde(rename = "startup_context_apply_status")]
    StartupContextApplyStatus {
        id: u64,
        status: StartupContextApplyStatus,
    },

    #[serde(rename = "startup_context_failed")]
    StartupContextFailed {
        id: u64,
        failure: StartupContextFailure,
    },

    /// Expanded compacted-history window (response to GetCompactedHistory).
    #[serde(rename = "compacted_history")]
    CompactedHistory {
        id: u64,
        session_id: String,
        messages: Vec<HistoryMessage>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<jcode_session_types::RenderedImage>,
        compacted_total: usize,
        compacted_visible: usize,
        compacted_remaining: usize,
        #[serde(default)]
        compacted_hidden_prompts: usize,
    },

    /// Bounded authoritative snapshot for the context editor.
    #[serde(rename = "context_editor_snapshot")]
    ContextEditorSnapshot {
        id: u64,
        snapshot: ContextEditorSnapshot,
    },

    /// One image-safe lazy detail chunk for a stored content block.
    #[serde(rename = "context_message_detail")]
    ContextMessageDetail {
        id: u64,
        detail: ContextMessageDetail,
    },

    /// Authoritative structural-closure preview for staged summary ranges.
    #[serde(rename = "context_range_closure_preview")]
    ContextRangeClosurePreview {
        id: u64,
        preview: ContextRangeClosurePreview,
    },

    /// Exact role prompts, source scope, and limits for every pending atomic curator call.
    #[serde(rename = "context_curator_plan_preview")]
    ContextCuratorPlanPreview {
        id: u64,
        preview: ContextCuratorPlanPreview,
    },

    /// The durable curator default was written successfully and resolved against this session.
    #[serde(rename = "context_curator_default_saved")]
    ContextCuratorDefaultSaved {
        id: u64,
        selection: ContextCuratorSelection,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolved_route: Option<ContextCuratorRoutePreview>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unavailable_reason: Option<String>,
    },

    /// Context draft preparation progress. The request ID is retained across
    /// every update emitted for the request that attached this monitor.
    #[serde(rename = "context_draft_progress")]
    ContextDraftProgress {
        id: u64,
        draft_id: String,
        progress: ContextDraftProgress,
    },

    /// Complete ready draft for final review.
    #[serde(rename = "context_draft_ready")]
    ContextDraftReady { id: u64, draft: Box<ContextDraft> },

    #[serde(rename = "context_draft_applying")]
    ContextDraftApplying {
        id: u64,
        identity: ContextDraftIdentity,
    },

    /// Non-stale preparation or apply failure.
    #[serde(rename = "context_draft_failed")]
    ContextDraftFailed {
        id: u64,
        identity: ContextDraftIdentity,
        error: ContextServiceError,
    },

    /// Draft identity no longer matches the authoritative transcript or route.
    #[serde(rename = "context_draft_stale")]
    ContextDraftStale {
        id: u64,
        identity: ContextDraftIdentity,
        error: ContextServiceError,
    },

    #[serde(rename = "context_draft_canceled")]
    ContextDraftCanceled {
        id: u64,
        identity: ContextDraftIdentity,
    },

    #[serde(rename = "context_draft_expired")]
    ContextDraftExpired {
        id: u64,
        identity: ContextDraftIdentity,
    },

    /// Retained terminal draft status returned after reconnect.
    #[serde(rename = "context_draft_applied")]
    ContextDraftApplied {
        id: u64,
        identity: ContextDraftIdentity,
        transaction_id: String,
        revision: u64,
    },

    /// Exact ready-draft preview after changing the selected distillation subset.
    #[serde(rename = "context_draft_selection_preview")]
    ContextDraftSelectionPreview {
        id: u64,
        preview: ContextDraftSelectionPreview,
    },

    #[serde(rename = "context_transaction_history")]
    ContextTransactionHistory {
        id: u64,
        context_revision: u64,
        total_transactions: usize,
        offset: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_offset: Option<usize>,
        transactions: Vec<ContextTransactionSummary>,
    },

    /// Complete persisted transaction detail for provenance inspection.
    #[serde(rename = "context_transaction_detail")]
    ContextTransactionDetail {
        id: u64,
        detail: Box<ContextTransactionDetail>,
    },

    #[serde(rename = "context_transaction_applied")]
    ContextTransactionApplied {
        id: u64,
        draft_id: String,
        result: ContextTransactionResult,
    },

    #[serde(rename = "context_transaction_reverted")]
    ContextTransactionReverted {
        id: u64,
        transaction_id: String,
        result: ContextTransactionResult,
    },

    #[serde(rename = "context_transaction_reapplied")]
    ContextTransactionReapplied {
        id: u64,
        transaction_id: String,
        result: ContextTransactionResult,
    },

    /// Correlated rejection for context requests that could not safely execute.
    #[serde(rename = "context_request_rejected")]
    ContextRequestRejected {
        id: u64,
        request: ContextRequestKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        draft_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transaction_id: Option<String>,
        error: ContextServiceError,
    },

    /// Prompt-safe signal used by Step 10 when a request must not be sent.
    #[serde(rename = "context_action_required")]
    ContextActionRequired {
        id: u64,
        session_id: String,
        context_revision: u64,
        reason: ContextActionRequiredReason,
        required_reduction_tokens: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pending_input: Option<ContextPendingInputMetadata>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preflight: Option<ContextPreflightReport>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<ContextPayloadPressure>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        details: Vec<String>,
        #[serde(default)]
        automatic_retry: bool,
    },

    /// Session-scoped, non-transcript pressure state calculated from the full
    /// request shape before provider invocation.
    #[serde(rename = "context_pressure_updated")]
    ContextPressureUpdated {
        id: u64,
        session_id: String,
        report: ContextPreflightReport,
    },

    #[serde(rename = "context_emergency_policy_changed")]
    ContextEmergencyPolicyChanged {
        id: u64,
        session_id: String,
        policy: jcode_session_types::StoredContextEmergencyPolicy,
    },

    /// Side panel state changed for the active session
    #[serde(rename = "side_panel_state")]
    SidePanelState { snapshot: SidePanelSnapshot },

    /// Server is reloading (clients should reconnect)
    #[serde(rename = "reloading")]
    Reloading {
        /// New socket path to connect to (if different)
        #[serde(skip_serializing_if = "Option::is_none")]
        new_socket: Option<String>,
    },

    /// Progress update during server reload
    #[serde(rename = "reload_progress")]
    ReloadProgress {
        /// Step name (e.g., "git_pull", "cargo_build", "exec")
        step: String,
        /// Human-readable message
        message: String,
        /// Whether this step succeeded (None = in progress)
        #[serde(skip_serializing_if = "Option::is_none")]
        success: Option<bool>,
        /// Output from the step (stdout/stderr)
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },

    /// Model changed (response to cycle_model)
    #[serde(rename = "model_changed")]
    ModelChanged {
        id: u64,
        model: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider_name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    /// Reasoning effort changed (response to set_reasoning_effort)
    #[serde(rename = "reasoning_effort_changed")]
    ReasoningEffortChanged {
        id: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    /// Service tier changed (response to set_service_tier)
    #[serde(rename = "service_tier_changed")]
    ServiceTierChanged {
        id: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        service_tier: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    /// Transport changed (response to set_transport)
    #[serde(rename = "transport_changed")]
    TransportChanged {
        id: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        transport: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    /// Available models updated (pushed after auth changes)
    #[serde(rename = "available_models_updated")]
    AvailableModelsUpdated {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_model: Option<String>,
        available_models: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        available_model_routes: Vec<jcode_provider_core::ModelRoute>,
    },

    /// Notification from another agent (file conflict, message, shared context)
    #[serde(rename = "notification")]
    Notification {
        /// Session ID of the agent that triggered the notification
        from_session: String,
        /// Friendly name of the agent (e.g., "fox")
        #[serde(skip_serializing_if = "Option::is_none")]
        from_name: Option<String>,
        /// Type of notification
        notification_type: NotificationType,
        /// Human-readable message describing what happened
        message: String,
    },

    /// External transcript text targeted at the active TUI input.
    #[serde(rename = "transcript")]
    Transcript { text: String, mode: TranscriptMode },

    /// Completed `!cmd` shell execution for a connected remote client.
    #[serde(rename = "input_shell_result")]
    InputShellResult { result: InputShellResult },

    /// Response to comm_read request
    #[serde(rename = "comm_context")]
    CommContext {
        id: u64,
        /// Shared context entries
        entries: Vec<ContextEntry>,
    },

    /// Response to comm_list request
    #[serde(rename = "comm_members")]
    CommMembers { id: u64, members: Vec<AgentInfo> },

    /// Response to comm_list_channels request
    #[serde(rename = "comm_channels")]
    CommChannels {
        id: u64,
        channels: Vec<SwarmChannelInfo>,
    },

    /// Response to comm_summary request
    #[serde(rename = "comm_summary_response")]
    CommSummaryResponse {
        id: u64,
        session_id: String,
        tool_calls: Vec<ToolCallSummary>,
    },

    /// Response to comm_status request
    #[serde(rename = "comm_status_response")]
    CommStatusResponse {
        id: u64,
        snapshot: AgentStatusSnapshot,
    },

    /// Response to comm_report request
    #[serde(rename = "comm_report_response")]
    CommReportResponse {
        id: u64,
        status: String,
        message: String,
    },

    /// Response to comm_plan_status request
    #[serde(rename = "comm_plan_status_response")]
    CommPlanStatusResponse { id: u64, summary: PlanGraphStatus },

    /// Response to comm_assign_task request
    #[serde(rename = "comm_assign_task_response")]
    CommAssignTaskResponse {
        id: u64,
        task_id: String,
        target_session: String,
    },

    /// Response to comm_task_control request
    #[serde(rename = "comm_task_control_response")]
    CommTaskControlResponse {
        id: u64,
        action: String,
        task_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_session: Option<String>,
        status: String,
        summary: PlanGraphStatus,
    },

    /// Response to comm_read_context request
    #[serde(rename = "comm_context_history")]
    CommContextHistory {
        id: u64,
        session_id: String,
        messages: Vec<HistoryMessage>,
    },

    /// Response to comm_spawn request
    #[serde(rename = "comm_spawn_response")]
    CommSpawnResponse {
        id: u64,
        session_id: String,
        new_session_id: String,
    },

    /// Response to comm_list_models request
    #[serde(rename = "comm_list_models_response")]
    CommListModelsResponse {
        id: u64,
        /// The coordinator's currently active model (spawn default when no
        /// override is configured or requested).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current_model: Option<String>,
        /// The configured `agents.swarm_model` pin, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        configured_swarm_model: Option<String>,
        /// All model routes known to the server (model + provider + auth
        /// method + availability + rough cost estimate).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        model_routes: Vec<jcode_provider_core::ModelRoute>,
    },

    /// Response to comm_await_members request
    #[serde(rename = "comm_await_members_response")]
    CommAwaitMembersResponse {
        id: u64,
        /// Whether the condition was met (false = timed out)
        completed: bool,
        /// Final status of each watched member
        members: Vec<AwaitedMemberStatus>,
        /// Human-readable summary
        summary: String,
        /// True when the wait was handed off to a detached background watcher.
        /// In that case `members`/`completed` describe the current snapshot, not
        /// a final result; completion is delivered later via notify/wake.
        #[serde(default)]
        background_started: bool,
    },

    /// Response to split request — new session created with cloned conversation
    #[serde(rename = "split_response")]
    SplitResponse {
        id: u64,
        new_session_id: String,
        new_session_name: String,
    },

    /// Response to resume_all_sessions — summary of which sessions were continued.
    #[serde(rename = "resume_all_result")]
    ResumeAllResult {
        id: u64,
        /// Number of live sessions that were continued by this request.
        resumed: usize,
        /// Number of live sessions inspected but skipped (idle/complete/busy).
        skipped: usize,
        /// Friendly names (or short ids) of the sessions that were continued.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        resumed_sessions: Vec<String>,
        /// Human-readable summary suitable for direct display.
        message: String,
    },

    /// A running command is waiting for stdin input from the user
    #[serde(rename = "stdin_request")]
    StdinRequest {
        /// Unique request ID for matching the response
        request_id: String,
        /// The last line(s) of output (the prompt, e.g. "Password: ")
        prompt: String,
        /// Whether the input should be masked (password field)
        #[serde(default)]
        is_password: bool,
        /// Tool call ID this is associated with
        tool_call_id: String,
    },
}
