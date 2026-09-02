# Production model-instruction inventory

**Status:** Phase 3 WP-01 baseline and migration ledger

**Implementation starting revision:** `6860452868b26ae534635a12753c789cd177b3ca`

**Starting tree:** `673d0bb405e2c3f5ec52522166b216c7a151f474`

**Observed runtime:** `jcode v0.75.91-dev (686045286, dirty)`, runtime identity `686045286-dirty-3737035cbece`, current/shared channels equal, canary passed

**Inventory date:** 2026-09-02 UTC

## Purpose and authority

This is the authoritative Phase 3 migration map for production human-readable text that Jcode sends to a model as instructions. It records the actual source owner, runtime consumer, delivery shape, dynamic values, scope behavior, exact-baseline route, target resource, owning work package, exclusions, and later disposition.

The inventory is source-traced, not keyword-derived. Searches were navigation aids. Every candidate below was followed to a production provider request, stored model-visible message, tool definition, or model-visible tool result. Human-only copy, tests, probes, structural protocol text, protected context-curator prompts, dormant memory prompts, provider-required text, and self-development mechanics remain listed with explicit exclusions so later packages do not rediscover or accidentally migrate them.

`Current source` means the implementation-starting revision above. Later migrations update `Final disposition` and `Final evidence` without rewriting this baseline as though it were current.

## Field definitions

- **Delivery:** true system prompt, dynamic system suffix, hidden user-authority message, ordinary synthetic user message, child-session startup prompt, system parameter for a specialist call, tool description/schema, or model-visible tool result.
- **Scope / values:** current global/project precedence and typed or runtime values interpolated into the text. Structural tags and framing are named separately.
- **Baseline route:** how to reproduce exact provider-visible bytes at the starting revision. `git show <revision>:<path>` recovers literal source assets. Builder routes identify the complete production renderer and representative inputs where source spelling is not the runtime output.
- **Target / owner:** accepted managed kind and stable ID, or a precise exclusion. WP-02 owns repository/import mechanics, WP-03 system composition, WP-05 skills, WP-06 notifications/control messages, and WP-07 workflow/specialist prompts.
- **Final disposition:** starts `pending`. Later packages close it with a managed resource/evidence reference, retained compatibility input, or confirmed exclusion.

## Current primary composition order

At the starting revision `jcode-base::prompt::build_system_prompt_split_with_capabilities` emits:

1. Project or global legacy base-system replacement, otherwise embedded `system_prompt.md`
2. Mermaid capability prose when enabled
3. Self-development mechanics for canary/self-dev sessions
4. Project `AGENTS.md`, then global `~/AGENTS.md`
5. Project prompt overlay, then global prompt overlay
6. Project preferred-tool guidance, then global preferred-tool guidance
7. Available-skill names and descriptions
8. Dynamic dormant-memory injection when globally enabled
9. Dynamic active-skill body
10. Dynamic current-turn reminder
11. Dynamic Swarm effort directive

The project-before-global paired ordering is current behavior, not accepted Phase 3 ordering. The accepted composer changes paired scope order to global before project while preserving each owned resource's bytes.

## Managed system, compatibility-source, and skill rows

| ID | Current source and consumer | Delivery, scope, values, framing | Baseline route | Target / owner | Final disposition | Final evidence |
|---|---|---|---|---|---|---|
| `INS-SYS-001` | `crates/jcode-base/src/prompt/system_prompt.md`; `DEFAULT_SYSTEM_PROMPT`; primary prompt builders in app-core and local TUI | Cacheable true-system text. Used only when no nonblank project/global replacement exists. Exact compatibility-agent body. | Literal asset SHA-256 in `instruction-inventory/EMBEDDED_ASSETS.sha256`; `git show` recovers 1,558 bytes. | `agent:global:jcode`; WP-03 | pending | pending |
| `INS-LEG-001` | `<project>/.jcode/system-prompt.md`; `prompt::load_base_system_prompt` | Live project-first full replacement. Blank/unreadable currently falls through. No wrapper. | Production builder with sandboxed project file; existing `project_system_prompt_file_replaces_default_base_prompt`. | Legacy import/compatibility source for `project:jcode`; WP-02/WP-03 | pending | pending |
| `INS-LEG-002` | `~/.jcode/system-prompt.md`; `prompt::load_base_system_prompt` | Live global full replacement after project candidate. Blank/unreadable falls through. | Production builder under sandboxed Jcode home. | Legacy import/compatibility source for `global:jcode`; WP-02/WP-03 | pending | pending |
| `INS-SYS-002` | `prompt::MERMAID_PROMPT`; `base_system_prompt_parts` | Cacheable true-system capability section, controlled by `features.mermaid`. No project source today. | `build_system_prompt_split_with_capabilities` with Mermaid enabled/disabled; existing mechanism test. | `system:mermaid`; WP-03 | pending | pending |
| `INS-EXT-001` | `<project>/AGENTS.md`; `load_agents_md_files_from_dir` | Dedicated live ecosystem input wrapped as `# Project Instructions (AGENTS.md)`. Current order before global. Empty present file still emits heading. | Production loader with exact file bytes. | Retain dedicated project `AGENTS.md`; WP-03 | pending | pending |
| `INS-EXT-002` | `~/AGENTS.md`; `load_agents_md_files_from_dir` | Dedicated live ecosystem input wrapped as `# Global Instructions (~/AGENTS.md)`. Current order after project. | Production loader under sandboxed home. | Retain dedicated global `AGENTS.md`; WP-03 | pending | pending |
| `INS-LEG-003` | `<project>/.jcode/prompt-overlay.md`; `load_prompt_overlay_files_from_dir` | Cacheable additive text wrapped as `# Project Prompt Overlay (.jcode/prompt-overlay.md)`. Project currently precedes global. | Production builder; existing overlay test. | Import to project system/common resource without duplicate contribution; WP-02/WP-03 | pending | pending |
| `INS-LEG-004` | `~/.jcode/prompt-overlay.md`; same loader | Cacheable additive text wrapped as `# Global Prompt Overlay (~/.jcode/prompt-overlay.md)`. | Production builder; existing overlay test. | Import to global system/common resource; WP-02/WP-03 | pending | pending |
| `INS-LEG-005` | `<project>/.jcode/preferred-tools.md`; `load_preferred_tools_files_from_dir` | Cacheable additive tool operating guidance wrapped as `# Project Preferred Tools (.jcode/preferred-tools.md)`. | Production builder; existing preferred-tools test. | Legacy compatibility input during WP-03, then `tool-guidance:project:preferred-tools`; WP-02/WP-07 | pending | pending |
| `INS-LEG-006` | `~/.jcode/preferred-tools.md`; same loader | Cacheable additive tool operating guidance wrapped as `# Global Preferred Tools (~/.jcode/preferred-tools.md)`. | Production builder; existing preferred-tools test. | Legacy compatibility input during WP-03, then `tool-guidance:global:preferred-tools`; WP-02/WP-07 | pending | pending |
| `INS-SYS-003` | `prompt.rs` lines 548-558; available-skill section builder | Cacheable true-system section. Values: sorted effective skill `name`, `description`. Exact heading, list format, and closing sentence are code-owned prose today. | Production split builder with synthetic skills. | `system:available-skills`; WP-05 | pending | pending |
| `INS-SKL-001` | `skill.rs`; global/plugin/project `SKILL.md` discovery and `Skill::get_prompt` | Global precedence: plugin, `~/.jcode/skills`, `~/.agents/skills`; project overlay: `.jcode`, `.agents`, `.claude`, loaded later and wins by name. Each active body is `# Skill: {name}\n\n{description}\n\n{body}`. | Exact selected `SKILL.md` plus `Skill::get_prompt`; existing discovery/precedence tests. | Managed `skill:<name>` plus external read-only skill sources; WP-05 | pending | pending |
| `INS-SKL-002` | `prompt.rs` active-skill wrapper; app-core `agent/prompting.rs`; TUI `turn_memory.rs` | Dynamic system suffix `# Active Skill\n\n{Skill::get_prompt()}`. Current source is reread per provider request. | Production split builder after slash/tool activation. | Active-skill composition slot; WP-05 | pending | pending |
| `INS-TGD-001` | `prompt/swarm_prompt.md`, project/global `.jcode/swarm-prompt.md`; `CommunicateTool::new` | Embedded into the `swarm` tool description after `Swarm prompt (user-tunable...)`. Current precedence project then global then embedded. Tool definitions freeze after registry construction. | Literal asset hash plus `CommunicateTool::new`; existing description/precedence tests. | `tool-guidance:swarm-routing`; WP-07, with legacy import in WP-02 | pending | pending |
| `INS-SYS-004` | `prompt::SWARM_EFFORT_DIRECTIVE`; `append_swarm_effort_directive` | Dynamic system suffix when effort is `swarm`. No project source. | Production split builder with effort sentinel; existing test. | `system:swarm-effort`; WP-07 | pending | pending |
| `INS-SYS-005` | `prompt::SWARM_DEEP_EFFORT_DIRECTIVE`; same consumer | Dynamic system suffix when effort is `swarm-deep`. | Production split builder with deep sentinel; existing test. | `system:swarm-deep-effort`; WP-07 | pending | pending |
| `INS-SYS-006` | `agent/prompting.rs::append_current_turn_system_reminder` and TUI equivalent | Structural dynamic-system heading `# System Reminder` followed by consumer-supplied current-turn prose. | Production prompt builders with synthetic reminder. | Heading/framing excluded as structural; each source row below owns its prose. WP-06/WP-07 | pending | pending |

## Notification and control-message rows

| ID | Current source and consumer | Delivery, scope, values, framing | Baseline route | Target / owner | Final disposition | Final evidence |
|---|---|---|---|---|---|---|
| `INS-NTF-001` | `prompt::build_session_context`; `Session::append_initial_session_context_message` | Hidden persisted user-authority message with `StoredDisplayRole::System`, wrapped in `<system-reminder>`. Values: date/time, OS, architecture, Jcode version, hardware, cwd, Git state. | `build_session_context` with fixed environment fixture; wrapper in `session.rs`. | `notification:session-context`; WP-06. Wrapper remains structural. | pending | pending |
| `INS-NTF-002` | `Session::append_fork_notice` | Hidden persisted user-authority message. Values: parent display name and ID. `<system-reminder>` wrapper is structural. | Direct builder call with representative IDs. | `notification:session-fork`; WP-06 | pending | pending |
| `INS-NTF-003` | `Session::append_transfer_handoff` | Persisted user-authority/system-display handoff wrapper. Values: source session ID and generated summary. | Direct builder with representative summary. | `notification:session-transfer-handoff`; WP-06. Summary generation is `INS-WFL-021/022`. | pending | pending |
| `INS-NTF-004` | `session/startup_context.rs::INITIAL_CONTROL_MESSAGE` | Hidden persisted user-authority control message before ordered startup file messages. XML tag is structural. | Literal source at starting revision; Startup Context production install test. | `notification:startup-context-initial`; WP-06 | pending | pending |
| `INS-NTF-005` | `LATE_UPDATE_CONTROL_MESSAGE` | Hidden persisted user-authority control message before late-added files. XML tag is structural. | Literal source and late-apply production tests. | `notification:startup-context-update`; WP-06 | pending | pending |
| `INS-NTF-006` | `stale_marker_text`, four active state sentences plus `STALE_MARKER_REMAINDER` | Hidden persisted user-authority/system-display stale notice. Values: path, startup SHA, observed state. XML and attributes remain Startup Context-owned. | Builder for changed/missing/unreadable/unsupported states; existing observation tests. | `notification:startup-context-stale`; WP-06 | pending | pending |
| `INS-NTF-007` | `message::generated_image_visual_context_blocks` | Hidden text plus image pixels. Values: path, safe MB limit, format, optional metadata path and revised prompt. `<system-reminder>` wrapper structural. | Production builder with image fixture; existing tests. | `notification:generated-image-visual-context`; WP-06 | pending | pending |
| `INS-NTF-008` | `Agent::BATCH_NUDGE` in `turn_loops.rs` | Ephemeral synthetic user message after repeated sequential tool rounds. Single-line `<system-reminder>` wrapper. | Production turn-loop threshold/availability path. | `notification:batch-nudge`; WP-06 | pending | pending |
| `INS-NTF-009` | `maybe_continue_empty_post_tool_response` | Persisted synthetic user message after empty provider response following tool results. | Direct helper with recording session; existing response-recovery test. | `notification:empty-post-tool-continuation`; WP-06 | pending | pending |
| `INS-NTF-010` | `continuation_prompt_for_stop_reason` / `maybe_continue_incomplete_response` | Persisted synthetic user message. Value: provider stop reason. Bracketed system-reminder label is current prose/framing. | Direct helper for supported stop reasons. | `notification:incomplete-response-continuation`; WP-06 | pending | pending |
| `INS-NTF-011` | `maybe_continue_stranded_tool_use` | Persisted synthetic user message when `tool_use` arrives with no parsed call. | Direct helper and recording-provider route. | `notification:stranded-tool-use`; WP-06 | pending | pending |
| `INS-NTF-012` | `FABLE_GUARDRAIL_RECONSIDERATION_PROMPTS` | Up to three persisted synthetic user messages after Fable guardrail stops. No dynamic values. | Direct helper with Fable model and guardrail stop reasons. | `notification:fable-guardrail-reconsideration-{1,2,3}`; WP-06 | pending | pending |
| `INS-NTF-013` | `ambient::format_scheduled_session_message`; all schedule targets in `ambient/runner.rs` | Stored/live system-display turn. Values: task/context, cwd, relevant files, branch, additional context. | Formatter with complete representative `ScheduledItem`; delivery matrix tests. | `notification:scheduled-task-due`; WP-06 | pending | pending |
| `INS-NTF-014` | `server/background_tasks.rs` background completion wake reminder | Dynamic system reminder paired with the formatted background completion message: review and continue if useful. | `dispatch_background_task_completion` with recording live turn. | `notification:background-task-completed`; WP-06 | pending | pending |
| `INS-NTF-015` | same file, Swarm await completion wake reminder | Dynamic system reminder paired with await result. | `dispatch_swarm_await_completion` with recording live turn. | `notification:swarm-await-completed`; WP-06 | pending | pending |
| `INS-NTF-016` | `comm_session.rs`: `You are the coordinator for this swarm.` | Server notification that becomes model-visible soft/system input under Swarm delivery. | Coordinator creation path. | `notification:swarm-coordinator-initial`; WP-06 | pending | pending |
| `INS-NTF-017` | `client_session.rs` and `server/swarm.rs`: `You are now the coordinator...` | Server notification on coordinator failover/promotion. | Coordinator removal/promote path. | `notification:swarm-coordinator-promoted`; WP-06 | pending | pending |
| `INS-NTF-018` | `todo::TODO_LONG_SESSION_REVIEW_MESSAGE` | Persisted synthetic user continuation after private long-session review trigger. | `take_long_session_review_if_due`; input state fixture. | `notification:todo-long-session-review`; WP-06 | pending | pending |
| `INS-NTF-019` | `TODO_INTENT_UNDERSTANDING_CONTINUATION_MESSAGE` | Synthetic user continuation after severe intent-understanding finding. | Todo tool/update path with synthetic assessment. | `notification:todo-intent-review`; WP-06 | pending | pending |
| `INS-NTF-020` | `TODO_CLOSED_FEEDBACK_LOOP_CONTINUATION_MESSAGE` | Synthetic user continuation directing feedback-loop repair. | Todo tool/update path. | `notification:todo-feedback-loop-review`; WP-06 | pending | pending |
| `INS-NTF-021` | `build_todo_ownership_continuation_message` | Synthetic user continuation. Values: completed goal labels and every failing ownership/verification facet. | Builder with synthetic todos/goals. | `notification:todo-ownership-review`; WP-06 | pending | pending |
| `INS-NTF-022` | `build_todo_completion_continuation_message` | Synthetic user continuation. Values: up to six todo labels plus overflow count. | Builder with synthetic todos. | `notification:todo-completion-review`; WP-06 | pending | pending |
| `INS-NTF-023` | `build_todo_confidence_spike_continuation_message` | Synthetic user continuation. Values: spike-classified todo labels. | Builder with synthetic confidence histories. | `notification:todo-confidence-recheck`; WP-06 | pending | pending |
| `INS-NTF-024` | `build_gate_digest` / `TODO_GATE_DIGEST_PREFIX` | Turn-end synthetic user continuation. Values: finding kind, goal group, repeat count, whether later cleared. | Builder across all observation variants. | `notification:todo-quality-digest`; WP-06 | pending | pending |
| `INS-NTF-025` | `todo::build_auto_poke_message` | Synthetic user continuation. Value: incomplete todo count/plural. | Builder and local/CLI auto-poke paths. | `notification:todo-auto-poke`; WP-06 | pending | pending |
| `INS-NTF-026` | `tool/config_edit_notice.rs` | Instructional suffix in model-visible `write`/`edit`/patch tool results. Values: path, parse error, changed settings and live/restart status. | `ConfigEditWatch` around active config edits; existing tests. | `notification:config-edit-result`; WP-06 | pending | pending |
| `INS-NTF-027` | `tool/browser.rs::firefox_status` and `ensure_firefox_ready` | Model-visible tool result/error guidance. Branches: ready, outdated extension, installed/unresponsive, absent, action retry blocked. Values: missing actions. | Browser status/readiness fixtures. | `notification:browser-readiness`; WP-06 | pending | pending |
| `INS-NTF-028` | `agent/turn_streaming_mpsc.rs::reload_interrupted_tool_result` | Synthetic tool result after reload. Branches: selfdev reload, wait-like call with serialized input, ordinary interrupted tool. Values: tool, elapsed, input. | Direct helper/stream recovery tests. | `notification:tool-interrupted-by-reload`; WP-06. Selfdev-specific branch remains excluded by `INS-EXC-006`. | pending | pending |
| `INS-NTF-029` | `tool/bash.rs` foreground timeout, reload persistence, and background launch result builders | Model-visible tool-result continuation guidance naming task ID/output/status files and `bg` follow-up actions. | Bash timeout/background/reload tests; exact output builder. | Notification family `bash-background-*`; WP-06 | pending | pending |
| `INS-NTF-030` | `tool/bg.rs` wait/status/output/cancel result builders | Model-visible lifecycle and next-action guidance for running/completed tasks and multi-wait. Values: task IDs/status/output. | `BgTool::execute` action matrix. | Notification family `background-task-*`; WP-06 | pending | pending |
| `INS-NTF-031` | `tool/communicate.rs` run-plan pause/failure/retention and credential-breaker hints | Model-visible Swarm tool-result recovery guidance. Values: task/plan/member IDs and failures. | Action-specific formatter tests. | Notification family `swarm-plan-recovery-*`; WP-06 | pending | pending |
| `INS-NTF-032` | `tool/discover.rs` listing/selection/error receipts | Model-visible guidance requiring selection before setup, forbidding invented setup, and explaining off-catalog behavior. Values: product/tool names and setup. | Discovery browse/select/suggest tests. | Notification family `integration-selection-*`; WP-06 | pending | pending |
| `INS-NTF-033` | `message/notifications.rs::format_background_task_notification_markdown`; local TUI and shared-server background completion delivery | Persisted ordinary user-authority message with `StoredDisplayRole::BackgroundTask` when a completion is delivered to an idle session. Values: task ID/label/status/duration/exit code/failure summary/output preview and `bg` retrieval command. Progress-only formatting is display-only and excluded by `INS-EXC-018`. | Production formatter plus local and shared-server delivery tests. | `notification:background-task-result`; WP-06 | pending | pending |

## Workflow and specialist rows

| ID | Current source and consumer | Delivery, scope, values, framing | Baseline route | Target / owner | Final disposition | Final evidence |
|---|---|---|---|---|---|---|
| `INS-WFL-001` | `prompt/mission_continuation.md`; `mission::render_mission_continuation_prompt` | Dynamic current-turn system reminder. Values: XML-escaped objective and long-horizon intent. | Literal asset hash plus renderer with representative `Mission`. | `module:mission-continuation`; WP-07 | pending | pending |
| `INS-WFL-002` | `commands.rs::build_commit_prompt` | Synthetic user turn from `/commit`. No dynamic values. | Direct builder. | `module:workflow-commit`; WP-07 | pending | pending |
| `INS-WFL-003` | `build_commit_push_prompt` | Synthetic user turn from `/commit-push`; composes commit prose plus push addendum. | Direct builder. | `module:workflow-commit-push`; WP-07 | pending | pending |
| `INS-WFL-004` | `build_release_prompt`, `build_fast_release_prompt` | Synthetic user release workflow. Values: fast-Linux preparation and publication instructions. | Direct builder. | `module:workflow-release-fast`; WP-07 | pending | pending |
| `INS-WFL-005` | `build_fast_macos_release_prompt` | Synthetic user release workflow, macOS arm64 variant. | Direct builder. | `module:workflow-release-fast-macos`; WP-07 | pending | pending |
| `INS-WFL-006` | `build_remote_release_prompt` | Synthetic user remote-CI release variant. | Direct builder. | `module:workflow-release-remote`; WP-07 | pending | pending |
| `INS-WFL-007` | `build_triage_prompt` | Synthetic user `/triage` workflow. Value: optional user focus appended verbatim. | Direct builder with empty/nonempty focus. | `module:workflow-github-triage`; WP-07 | pending | pending |
| `INS-WFL-008` | `build_test_verification_prompt` | Synthetic user `/test` verification orchestrator. Value: explicit claim or default target. | Direct builder. | `module:workflow-test-verification`; WP-07 | pending | pending |
| `INS-WFL-009` | `commands_plan.rs::build_plan_prompt` | Synthetic user `/plan` instruction. Value: explicit goal or inferred-session default. | Direct builder. | `module:workflow-plan`; WP-07 | pending | pending |
| `INS-WFL-010` | `commands_improve.rs::build_improve_prompt(plan_only=true)` | Synthetic user improvement-planning workflow. Optional focus. | Direct builder. | `module:workflow-improve-plan`; WP-07 | pending | pending |
| `INS-WFL-011` | `build_improve_prompt(plan_only=false)` | Synthetic user iterative implementation workflow. Optional focus. | Direct builder. | `module:workflow-improve`; WP-07 | pending | pending |
| `INS-WFL-012` | `build_refactor_prompt(plan_only=true)` | Synthetic user refactor-planning workflow. Optional focus. | Direct builder. | `module:workflow-refactor-plan`; WP-07 | pending | pending |
| `INS-WFL-013` | `build_refactor_prompt(plan_only=false)` | Synthetic user refactor implementation/review loop. Optional focus. | Direct builder. | `module:workflow-refactor`; WP-07 | pending | pending |
| `INS-WFL-014` | `improve_stop_prompt` and `refactor_stop_prompt` | Synthetic queued user instruction at next safe point. | Direct builders. | `notification:workflow-improve-stop` and `workflow-refactor-stop`; WP-07 | pending | pending |
| `INS-WFL-015` | `build_improve_resume_prompt` | Synthetic user resume instruction. Values: mode and incomplete todo rows. | Direct builder over all branches. | `module:workflow-improve-resume`; WP-07 | pending | pending |
| `INS-WFL-016` | `build_refactor_resume_prompt` | Synthetic user resume instruction. Values: mode and incomplete todo rows. | Direct builder over all branches. | `module:workflow-refactor-resume`; WP-07 | pending | pending |
| `INS-WFL-017` | `commands_review.rs::review_session_read_only_guardrails` | Shared child-session startup module for review/judge agents. | Direct function via startup builders. | `module:review-read-only-guardrails`; WP-07 | pending | pending |
| `INS-WFL-018` | `judge_session_visible_context_notice` | Shared child-session context caveat for judge/autojudge. | Direct function via judge builders. | `module:judge-visible-context`; WP-07 | pending | pending |
| `INS-WFL-019` | `build_autoreview_startup_message`, `build_review_startup_message` | First staged user prompt in cloned review child. Values: parent session ID; shared guardrails. | Direct builders plus child startup-message path. | `agent:autoreview-compat` / `agent:review-compat` or workflow resources as finalized by WP-07; WP-07 | pending | pending |
| `INS-WFL-020` | `build_autojudge_startup_message`, `build_judge_startup_message` | First staged user prompt in cloned judge child. Values: parent session ID; visible-context and read-only modules. | Direct builders plus cloned visible transcript route. | `agent:autojudge-compat` / `agent:judge-compat` or workflow resources; WP-07 | pending | pending |
| `INS-WFL-021` | `transfer_handoff.rs::TRANSFER_PROMPT`; `build_transfer_prompt` | User prompt to specialist completion. Preceded by rendered conversation and separator. Conversation is bounded by provider capacity. | Builder with representative transcript; existing tests. | `module:transfer-handoff-task`; WP-07 | pending | pending |
| `INS-WFL-022` | `build_transfer_handoff_summary` literal system `You prepare precise...` | Specialist true-system parameter for transfer summarizer. | Recording provider call. | `agent:transfer-handoff-summarizer`; WP-07 | pending | pending |
| `INS-WFL-023` | `jcode-sdk::structured_instructions` / `build_structured_prompt` | Appended user instructions for JSON-only structured output. Value: stable pretty JSON Schema. | SDK structured run with synthetic schema. | `module:structured-output`; WP-07 | pending | pending |
| `INS-WFL-024` | `build_structured_correction_prompt` | Retry user prompt. Values: schema, validation issues, truncated prior response. | Structured retry path with synthetic invalid response. | `notification:structured-output-correction`; WP-07 | pending | pending |
| `INS-WFL-025` | `ambient/prompt.rs::build_ambient_system_prompt` | Ambient true-system override. Values: state, schedule queue, sessions, resource budget, directives. Memory sections are dormant/excluded while memory is disabled. | Builder with representative non-memory fixtures; existing ambient tests. | `agent:ambient-compat` plus modules; WP-07 | pending | pending |
| `INS-WFL-026` | `jcode-overnight-core::build_coordinator_prompt` | Headless overnight coordinator prompt. Values: run/times, mission, paths, preflight. | Direct builder with representative manifest/preflight. | `agent:overnight-coordinator`; WP-07 | pending | pending |
| `INS-WFL-027` | `build_visible_current_session_prompt` | Visible current-session overnight coordinator prompt. Values: manifest paths/times/mission. | Direct builder. | `agent:overnight-visible-coordinator`; WP-07 | pending | pending |
| `INS-WFL-028` | `build_continuation_prompt` | Overnight continuation synthetic turn. Values: phase, target relation, next prompt. | Direct builder across phases. | `notification:overnight-continuation`; WP-07 | pending | pending |
| `INS-WFL-029` | `build_handoff_ready_prompt` | Pre-wake handoff reminder. Value: review-notes path. | Direct builder. | `notification:overnight-handoff-ready`; WP-07 | pending | pending |
| `INS-WFL-030` | `build_morning_report_prompt` | Target-time morning report instruction. Value: review path. | Direct builder. | `notification:overnight-morning-report`; WP-07 | pending | pending |
| `INS-WFL-031` | `build_post_wake_continuation_prompt` | Post-wake bounded-continuation instruction. Values: review path, grace end. | Direct builder. | `notification:overnight-post-wake`; WP-07 | pending | pending |
| `INS-WFL-032` | `build_final_wrapup_prompt` | Final cleanup/completion instruction. Value: review path. | Direct builder. | `notification:overnight-final-wrap`; WP-07 | pending | pending |
| `INS-WFL-033` | TUI `commands_overnight.rs::build_overnight_poke_message` | Synthetic user auto-poke. Values: run/artifact paths, phase, stalled turns. | Builder across all `OvernightPokePhase` branches. | `notification:overnight-auto-poke`; WP-07 | pending | pending |
| `INS-WFL-034` | `server/swarm.rs::run_swarm_message` planner prompt | Synthetic user prompt to current agent: decompose request into 2-4 JSON subtasks. Values: cwd and request. | Recording agent/provider around `run_swarm_message`. | `module:swarm-task-planner`; WP-07 | pending | pending |
| `INS-WFL-035` | same function, integration prompt | Synthetic user prompt to current agent after worker results. Values: original request and all outputs. | Recording route with synthetic task outputs. | `module:swarm-result-integrator`; WP-07 | pending | pending |
| `INS-WFL-036` | `jcode-swarm-core::append_swarm_completion_report_instructions` | Structural `<system-reminder>` appended to worker assignment. No dynamic values beyond original assignment. | Direct builder/idempotency test. | `notification:swarm-worker-report-contract`; WP-07 | pending | pending |
| `INS-WFL-037` | `append_deep_node_instructions` | Structural worker assignment reminder. Values: node ID, member cap; separate expandable/non-expandable branches. | Direct builder tests. | `notification:swarm-deep-node-contract`; WP-07 | pending | pending |
| `INS-WFL-038` | `append_deep_gate_instructions` | Structural critique-gate reminder. Values: gate ID, audited IDs, low-confidence siblings. | Direct builder tests. | `notification:swarm-deep-gate-contract`; WP-07 | pending | pending |
| `INS-WFL-039` | `server/comm_control.rs::composite_synthesis_content` | Re-woken composite-node synthesis instruction. Values: node ID and original brief. | Deep-plan assignment test. | `notification:swarm-composite-synthesis`; WP-07 | pending | pending |

## Explicit exclusions and retained code-owned boundaries

| ID | Source family | Provider/model visibility and evidence | Exclusion reason / retained owner | Final disposition | Final evidence |
|---|---|---|---|---|---|
| `INS-EXC-001` | Context range summarizer: `context/curator.rs::{RANGE_SUMMARIZER_BASE_PROMPT,RANGE_MANDATORY_INSTRUCTIONS}` | True-system prompt for isolated context-curator call. Versioned and covered by context-control tests. | Protected accepted context-control architecture. Remains owned and versioned by context curator. | excluded | starting revision source and `PROGRESS-50.md` |
| `INS-EXC-002` | Context tool-result distiller: `TOOL_DISTILLER_BASE_PROMPT`, `TOOL_MANDATORY_INSTRUCTIONS` | True-system prompt for isolated context-curator call. | Same protected boundary. | excluded | starting revision source and context-control acceptance docs |
| `INS-EXC-003` | User-approved context transaction/range instructions and curator JSON schemas | Added to curator system prompt as user data; schema is structural output contract. | User-authored per-operation data and protected structural contract, not managed central prose. | excluded | `context/curator.rs::effective_prompt` |
| `INS-EXC-004` | Dormant memory prompts: reranker, relevance checker, contradiction detector, extractor, memory formatting/injection | Model-facing only when memory pipeline is active. Phase 2 hard-disable prevents retrieval, injection, extraction, storage, and tools. | Dormant memory implementation and data are protected. Phase 3 must not discover them as active managed content. | excluded | `docs/MEMORY_POLICY.md`, Phase 2 completion |
| `INS-EXC-005` | Self-development prompt assets: `selfdev_mode.txt`, TUI and desktop focus files | Cacheable true-system text only for canary/self-dev sessions. Asset hashes recorded. | Self-development mechanics are explicitly code-coupled and excluded from managed instructions. | excluded | asset hash manifest and `prompt.rs` |
| `INS-EXC-006` | Selfdev reload/recovery continuation prose in `tool/selfdev/reload.rs` | Persisted/synthetic continuation messages and tool-result guidance after reload. | Self-development lifecycle mechanics; exact coupling to reload/build/task recovery. | excluded | `ReloadContext` builders and selfdev tests |
| `INS-EXC-007` | Anthropic OAuth billing header | First provider system block. | Provider-required billing attribution. | excluded | `jcode-provider-anthropic::build_system_param_split` |
| `INS-EXC-008` | Anthropic/sidecar OAuth identity text | Provider system blocks, including dormant sidecar identities. | Provider/auth-route-required identity and compatibility text. | excluded | Anthropic provider and `sidecar.rs` |
| `INS-EXC-009` | Anthropic/Copilot/Bedrock trailing `Continue.` repairs and missing-tool-result placeholders | Provider-added conversation repair. | Protocol compatibility required by provider message grammar. | excluded | provider formatters and validation tests |
| `INS-EXC-010` | Gemini function-call guard | Appended to Gemini system instruction only when tools are advertised. | Provider-specific mechanically coupled tool-protocol prevention. | excluded | `build_system_instruction_with_tool_guard` |
| `INS-EXC-011` | ChatGPT web transport headings, untrusted-data notice, and `<jcode_tool_call>` contract | Flattened web prompt wrapping system, tools, and conversation. | Provider transport protocol inseparable from parser/bridge. | excluded | `chatgpt_web::build_web_prompt` |
| `INS-EXC-012` | Cursor CLI flattened prompt labels and truncation marker | Provider-native flattened string. | Provider transport representation and fixed native prompt cap, not ordinary managed instruction prose. | excluded | `cursor-runtime::build_cli_prompt` |
| `INS-EXC-013` | Provider failover prompt/error encoding | Shown to human/TUI to choose failover; not sent as model instruction. | Human permission/control surface. | excluded | `jcode-provider-core/src/failover.rs`, TUI model context |
| `INS-EXC-014` | Built-in tool descriptions and parameter descriptions | Model-visible in tool schemas. Implementations listed under `jcode-app-core/src/tool/*`, MCP, and provider-native schema adapters. | Explicitly excluded because prose is mechanically tied to schemas, permissions, validation, and dispatch. Future tool operating guidance uses managed `ToolGuidance` instead. | excluded | `Tool::to_definition`, registry tool list, schema tests |
| `INS-EXC-015` | Dynamic MCP tool name/description/schema supplied by MCP servers | Model-visible tool definitions after registration. | External protocol-owned schema content; tool locking and one intentional late-MCP rebuild remain authoritative. | excluded | MCP registry and issue-206 tests |
| `INS-EXC-016` | Structural tags and wrappers: `<system-reminder>`, Startup Context XML, Swarm markers, tool-call envelopes, headings that identify transport sections | Provider-visible structure around owned prose. | Delivery owner retains framing and stable structural identity. Managed resources return prose only. | excluded | each owning builder above |
| `INS-EXC-017` | User/project/file/task data inserted into prompts: Startup file bodies, mission objective, scheduled task body, Swarm assignment, tool outputs, user focus, schema JSON | Provider-visible data at runtime. | Authoritative user/runtime data, not Jcode-authored reusable instruction prose. Typed values feed registered consumers. | excluded | typed builder inputs in rows above |
| `INS-EXC-018` | Human-only TUI copy, command help, status notices, error strings, confirmations, onboarding, manager labels | Never sent to provider unless a user copies it manually. | Not model instructions. | excluded | TUI call-site tracing |
| `INS-EXC-019` | Tests, snapshots, fixtures, fuzz corpus, examples, and provider-doctor live probe prompts | Test/probe models may see the text, but it is not production agent behavior. | Tests and probes explicitly excluded. Provider-doctor prompt text remains diagnostic fixture authority. | excluded | source paths under tests and `jcode-provider-doctor` |
| `INS-EXC-020` | Desktop-only source and text | Desktop source excluded from Phase 3 product focus and WP-01. | Explicit package boundary. | excluded | protected desktop paths and Phase 3 dossier |
| `INS-EXC-021` | Endorsed-skill catalog descriptions in `ENDORSED_SKILLS` | Human `/skills` discovery metadata. Not part of effective skill prompt unless an actual installed `SKILL.md` supplies it. | Human-only catalog source. Managed/external active skills are `INS-SKL-001`. | excluded | `skill.rs`, skill-tool listing |
| `INS-EXC-022` | Legacy todo continuation strings retained only for transcript classification | Not newly emitted; matched so persisted old synthetic messages are not rendered as user prompts. | Compatibility recognition data, not active prose. | excluded | `todo::is_auto_poke_message` |
| `INS-EXC-023` | Raw background/Swarm/user soft-interrupt content | Passed through from tools, workers, or user/system callers. | Runtime source data; fixed wake/reminder prose is inventoried separately. | excluded | interrupt queues and server notification paths |
| `INS-EXC-024` | Image/tool/result structural summaries and missing-output repair markers | Provider-visible representations such as `[Image]`, tool-use/result labels, and interrupted-result placeholders. | Message/protocol structure coupled to provider validation and replay. | excluded | message/provider formatters |

## Tool-schema exclusion coverage

The code-owned schema exclusion covers every production `Tool` implementation found at the starting revision:

`agentgrep`, `schedule`/ambient permission tools, `apply_patch`, `bash`, `batch`, `bg`, `browser`, `swarm`, macOS computer use, `conversation_search`, `debug_socket`, `integration_tools`, `edit`, `gmail`, `initiative`, `invalid`, `jcode_docs`, `ls`, MCP management and MCP-provided tools, dormant `memory`, `multiedit`, `open`, `patch`, `read`, `selfdev`, `session_search`, `side_panel`, `skill_manage`, `todo`, `webfetch`, `websearch`, and `write`.

This coverage is not a prose snapshot. It records the explicit architecture decision that built-in schema descriptions remain with schema/permission/dispatch owners. WP-07 may add separate managed operating guidance for selected tools without moving their schemas.

## Exact-preservation baseline

### Literal embedded assets

`docs/dev/instruction-inventory/EMBEDDED_ASSETS.sha256` records SHA-256 and byte length for every production embedded instruction asset at the starting revision. These hashes are one-time migration evidence, not permanent prompt wording tests.

### Rust constants and composed builders

Exact bytes remain reproducible from the immutable starting commit and the named production builder for every row:

```bash
git show 6860452868b26ae534635a12753c789cd177b3ca:<path>
```

For composed rows, later packages capture representative pre-migration output by calling the named builder with the row's typed variables immediately before migration, then compare it with the managed renderer once. The equality artifact belongs in the package evidence and this ledger's `Final evidence` cell. It must not become a permanent phrase assertion or approved-prose snapshot test.

### Baseline mechanism results

- Full `jcode-base --lib` baseline: 1,331 passed, 16 failed, 1 ignored. The failures were pre-existing under the Phase 2 disabled-memory configuration and concurrent current-directory interference. They included 12 dormant memory/goal tests, three session-context cwd tests, and one skill cwd test.
- Isolated serial prompt tests: passed.
- Isolated serial skill tests: passed.
- Design-session prompt-freshness production probe: editing project `AGENTS.md` and prompt overlay changed the next render in the same process. This confirms current source contradicts the old documentation claim that sessions already freeze the prompt.

## Discovery coverage and reconciliation rule

The audit traced:

- `jcode-base` prompt, session, Startup Context, skill, todo, message, transfer, provider, sidecar, and memory builders
- app-core primary request construction, response recovery, interrupts, context curator, ambient/schedule, background completion, Swarm server, tools, and live-turn delivery
- local TUI prompt composition and command/workflow builders
- overnight core
- SDK structured output
- provider system construction and prompt flattening
- all built-in tool schema implementations
- test/probe and desktop-only candidates for exclusion

WP-06 and WP-07 migrate by row, not by repeating a broad keyword audit. If implementation work discovers a genuine production instruction missing here, the package must add a row with evidence, owner, and disposition before migration. WP-11 manually reconciles every row. No row may remain `pending`, `unknown`, or unowned at Phase 3 closeout.
