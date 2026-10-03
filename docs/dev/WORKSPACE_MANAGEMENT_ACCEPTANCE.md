# Workspace management acceptance ledger

**Candidate implementation evidence, 2026-10-03.** Covers SP-58-C01/WP-10
(R36, R37): the `/workspace` and `/runtime` management mode described in
[WORKSPACE_MANAGEMENT.md](../WORKSPACE_MANAGEMENT.md). Mirza's acceptance,
activation of the user's runtime and downstream publication are separate steps
recorded in the external completion report.

Native acceptance is macOS arm64. Every native journey uses an isolated state
root, an owned fixture daemon, disposable Git fixtures and a scripted localhost
provider that is never asked for a real answer. No paid inference, real
checkout, real catalog or the user's runtime was touched.

## Exercised boundary

Branch `mirza/sp58-c01-wp10-workspace-management` from `90af8cfef`. The native
journey ran against an immutable copy of the `8be1abf1c` selfdev build
(SHA-256 `34d348b910b4cc99ef72b4f736aba42616fd58f06bf2964d3295106ac3a8af24`).
Later commits change only the journey script and documentation.

Journey: `scripts/run_isolated_test.py python3
scripts/verify_workspace_manager_tui.py --binary <image> --artifact-dir <dir>`.
It drives a real TUI client in a PTY through debug-tester key and mouse
injection, captures rendered frames, and checks outcomes against the daemon's
public protocol and the CLI. `WORKSPACE_JOURNEY=runtime` runs only the attach,
layout and runtime stages for fast iteration. Testers are spawned by a separate
harness daemon that owns their PTY masters, so crashing the daemon under test
leaves the client's terminal drained, as a real terminal emulator would.

Synthetic reducer and render tests live in
`crates/jcode-tui/src/tui/workspace_manager/tests.rs`, `form.rs` and
`crates/jcode-tui/src/tui/app/tests/workspace_manager.rs`. Backend read
adapters are tested in `crates/jcode-base/src/workspace/operations_tests.rs`,
`crates/jcode-protocol/src/workspace_tests.rs` and the app-core location tests.

## Backend additions made for the human client

The client needed three read-only adapters to recover state without private
files or remembered IDs. All are additive and negotiated:

| Adapter | Purpose | Capability |
|---|---|---|
| `workspace/operations` | Newest-first paged discovery of clone, closeout, Startup Context copy, launch and location operations by target, session, kind and unfinished state | `management_version = 1` |
| `workspace/startup_copy_plans` | Current source/target Startup Context plan revisions for an exact copy review | `management_version = 1` |
| `primary_location/inspect_session` | Session placement, cwd, initial cwd, revision, legacy working directory, pending moves; catalog unavailability reported separately | `session_inspection_version = 1` |

A never-initialized catalog is reported as a typed `Workspace is not
initialized` issue (distinct from damage), and an interrupted initialization
carries its original request ID so the client resumes it rather than starting
a new one. Older clients still decode both.

## R36: organization, provisioning, launch, move and history

| Subcriterion | Evidence | Disposition |
|---|---|---|
| Uninitialized catalog: reviewed initialize, decline does nothing | Journey stage 1 (decline leaves `status` an error, confirm initializes); `uninitialized_catalog_offers_reviewed_initialize_only`, `interrupted_initialization_resumes_its_own_request_and_damage_offers_nothing`; base `only_a_never_initialized_catalog_reports_not_initialized` | Verified |
| Projects, logical repositories, flat work areas, associations, directory references | Journey stage "organization" through physical keys; catalog checked by public `list` | Verified |
| Disk/volume selection: saved default, default and custom destinations | Journey "volumes": data-volume default saved, `tool-custom` cloned to a custom path, `tool-default` to the saved default; real Git files present, no `alternates` | Verified |
| Clone review, progress, completion and recovery discovery | Same stage; `Operations` lists completed clones; `clone_form_requires_a_volume_and_builds_exact_spec` | Verified |
| Managed launch and standalone launch with explicit cwd | Journey "launch a managed primary into the custom clone"; "projectless standalone launch": project placement with empty cwd is refused before review, a new empty directory becomes its own standalone root with no home; `a_new_standalone_directory_is_its_own_root` | Verified (fixture enables `managed_primary_launch`) |
| Queued move keeping cwd at the work-area level | Journey "move the launched session to the work area"; `session_move_binds_session_and_catalog_revisions_and_staging_is_honest` | Verified |
| Legacy session review and adoption | Journey "adopt the tester's legacy session into the Docs directory" (exact recorded working directory) | Verified |
| Closed checkout history, closed visibility filter | Journey closeout stage: history frame shows `Closed`, preservation directory and report; Organization `Closed` filter lists the checkout | Verified |
| Stable IDs, paging, stale-cursor restart | `organization_rows_select_by_stable_identity_and_prefill_actions`, `known_entities_follow_every_page_and_a_stale_cursor_restarts_the_list`, `large_closeout_inventories_page_and_a_refreshed_digest_starts_over`; base `operation_pages_are_newest_first_and_reject_stale_continuations` | Validated |
| Drafts survive rejection, conflict, reconnect and resize | `rejected_or_declined_review_reopens_the_draft_and_conflict_refreshes_revision`, `drafts_survive_reconnect_and_resize`; earlier native runs reproduced a lost draft after a clone advanced the catalog, and the final run met no conflict once reviews bind the newest listed revision (any conflict is recorded in `conflicts.json` and re-reviewed); the journey crash stage keeps a typed draft across a daemon SIGKILL and reconnect | Verified |
| Layouts 120×32, 80×24, 60×24, 48×12, smaller | Journey resizes the real PTY (`tester:<id>:resize`) and captures each frame; 40×10 shows `needs 48×12`; `frames_are_usable_at_wide_standard_narrow_minimum_and_too_small`, `a_long_refusal_is_fully_visible_below_the_form_fields` | Verified |
| Mouse | Journey clicks a section tab and a footer action at their rendered cells; `mouse_selects_tabs_rows_and_footer_actions` | Verified |
| Keys are owned by the manager, not the composer | `workspace_manager_local_command_traps_keys_and_paste_without_touching_session`, `workspace_manager_remote_physical_keys_send_only_management_requests`; enhanced-keyboard shifted letters select actions | Validated |

## R37: grants, closeout, backup and runtime controls before agent exposure

| Subcriterion | Evidence | Disposition |
|---|---|---|
| Access proposal approved, then revoked | Journey: a proposal created through the public permission API is approved in the manager (prefilled exact audience and target), the grant is active, then revoked; `proposal_approval_prefills_exact_audience_and_target`, `directory_proposal_prefills_its_listed_choice_and_newer_pages_advance_the_review_revision` | Verified |
| Direct grant and reviewed carry on Clear | Journey "direct grant to this session, then Clear with a reviewed carry choice" | Verified (fixture enables `managed_primary_launch`) |
| Closeout with final human approval | Journey on `tool-default`: begin, inventory, one recorded human disposition, verified full-archive preservation, removal review with no issues, typed `approve`, finish; directory removed, closed history retained | Verified |
| Conditional no-loss authorization path | Journey on `tool-custom`: begin with the checkbox on (recorded `conditional_no_loss = true`), then revoke; files untouched. The agent-side declaration route is WP-11's | Verified for the human side |
| Backup, export, import, restore | Journey: named backup; export; importing a project whose locations are bound here is refused with the reason visible and the draft kept; a location-free export imports as new identities (grants disabled); typed `restore` of the named snapshot | Verified |
| Shutdown: finish-current waits and is cancelled while waiting | Journey: a held provider turn keeps a finish-current Stop in `WaitingForCurrent`, the review lists the primary turn, Cancel restores admission | Verified |
| Reviewed Stop, durable offline intent, Start from the TUI | Journey: Stop from the manager; client shows `intentionally stopped` from local state, offers only Start; no daemon is listening and the client does not restart it; `S` runs `jcode runtime start` and the client reconnects | Verified |
| Reviewed Restart | Journey: Restart from the manager reaches a new runtime incarnation without intentional Stop; client reconnects | Verified |
| Force needs a typed word; change and retry | `runtime_stop_review_confirm_and_force_needs_typed_word` (Blocked operation offers Force; an offline client offers no operation controls) | Validated |
| Crash recovery selection | Journey: daemon SIGKILL during a held turn; after Start the interrupted turn is listed `Needs decision`; Leave stopped resolves it once; `recovery_decision_binds_item_revision_and_one_request` | Verified |
| Uncertain effects are never resent automatically | `confirmed_effect_uncertainty_retries_the_same_request`, `foreign_and_stale_replies_cannot_change_state` | Validated |

## Defects found by the native journey and fixed

Each has a focused test and a later passing native stage.

- A clone advancing the catalog made the next review conflict and lost the
  draft; drafts now survive and reviews bind the newest revision a list showed.
- A directory target was prefilled with a checkout prefix.
- Identical outcome texts confused correlation; outcomes carry a sequence.
- Closeout inventories above 200 entries, the closeout list and entity lists
  beyond their first page were unreachable; all page now.
- A new standalone directory required a separate root; it is its own root.
- Long refusals were cut off in forms; text wraps to exact rows and the error
  and key hints stay visible.
- Remote onboarding prompts took keys from full-screen managers.
- A reconnecting client ignored debug-tester commands.
- An offline client offered live shutdown controls.

## Limits

- Managed launch, location change, legacy adoption and scoped new contexts stay
  behind `features.managed_primary_launch`, which the user's configuration keeps
  off; journeys enable it in their fixture only. WP-12 owns the rollout.
- The agent-side discovery and access-proposal tools and the conditional
  closeout declaration are not exposed (WP-11).
- Same-user trusted clients are not proof of a physical human; native write
  scope is not a shell sandbox.
- The final command-center visual design is SP-58-C04's.
- The full parallel `jcode-tui` suite did not finish within 90 minutes. Of its
  18 failures, the same tests rerun on `90af8cfef` fail identically, except two
  order-sensitive tests that fail intermittently on either side. Eight tests
  that exceeded 60 seconds under parallel load pass serially on both the
  candidate and `90af8cfef`. Parallel `jcode-base workspace::` runs show
  lock-contention `Busy` failures that pass serially (23 of 23).
- Focused `jcode-app-core` filters (`workspace::`, `primary::location`,
  `server::debug`): 39 of 41 passed in parallel. One Startup Context copy test
  hit process-global runtime admission set by a parallel shutdown test and
  passes alone; `swarm_debug_help_text_mentions_core_swarm_sections` is the
  recorded expected failure under the disabled-Swarm policy
  ([triage](EXECUTION_FAILURE_TRIAGE.json)).
