# Startup Context Verification Guide

This is the reproducible requirement-to-evidence guide for Jcode's Startup Context
capability. It complements the current user and maintainer reference in
[`../STARTUP_CONTEXT.md`](../STARTUP_CONTEXT.md).

The guide serves four purposes:

1. Map every Startup Context requirement to current source ownership and checks.
2. Define the final cross-package integration journeys.
3. Preserve the privacy, cache, caller, recovery, and context-control boundaries that a
   future change must recheck.
4. Give a fresh maintainer a complete verification route without reconstructing the
   feature from chat history.

## Verification identity

The integrated implementation entering the final documentation and acceptance-preparation
work was:

```text
commit: 0823779975ddd8d9ab5a8a5c0338f53e029c189e
subject: test(startup-context): verify privacy and context integration
```

The complete feature range begins after:

```text
b168305144fa7da49e9411257076696f05d08ddc
```

and contains the WP-01 through WP-10 implementation commits plus subsequent shipped
documentation and acceptance-preparation commits.

The source checkout is expected to retain these unrelated protected paths unless their
owner handles them separately:

```text
 M crates/jcode-desktop2/src/scene.rs
 M crates/jcode-desktop2/src/tests/actions.rs
?? .DS_Store
?? assets/.DS_Store
?? assets/demos/.DS_Store
```

Startup Context work must not format, stage, commit, delete, or otherwise modify those
paths.

This guide records verification mechanics and evidence boundaries. Final phase acceptance
and its exact final source/runtime identity belong in the external phase completion record.

## Evidence rules

- Use synthetic content to verify loading, capture, ordering, message construction,
  persistence, transport, correlation, cache placement, export omission, context
  projection, and failures.
- Do not snapshot, keyword-test, or behaviorally grade the user-approved control,
  late-update, or stale-marker prose.
- `Session.messages` is the authoritative source for provider requests and `/context`.
  Remote History and exports are separate projections.
- Prefer recording providers, production request builders, real shared-server stream
  pairs, public CLIs, and SDK clients over live model spending.
- Run environment-sensitive app-core suites serially.
- Run reload-state-mutating suites before the final build-reload.
- Report an unavailable platform or external boundary honestly. Do not relabel a
  substitute as runtime acceptance.

## Requirement map

| ID | Required behavior | Primary source ownership | Representative evidence |
|---|---|---|---|
| R-01 | Git common-directory identity, active worktree roots, non-Git canonical identity | `jcode-base::startup_context::project` | `git_subdirectories_share_identity_and_worktrees_share_plan_key`; `one_saved_relative_plan_captures_active_worktree_contents`; `non_git_identity_uses_canonical_launch_directory` |
| R-02 | Private ordered defaults, revision, backup recovery, owner-only storage | `jcode-base::startup_context::plan` | `missing_plan_is_empty_and_saved_plan_contains_no_file_contents`; corruption, revision, schema, identity, and permission tests in the base Startup Context family |
| R-03 | Explicit ordered selection and concrete non-recursive directory expansion | `jcode-base::startup_context::selection` and `browser` | `directory_expansion_is_direct_concrete_and_naturally_ordered`; TUI direct-directory selection and pagination tests |
| R-04 | Complete stable UTF-8 capture and complete-source bounds | `jcode-base::startup_context::capture` | exact Unicode, unsupported source, large bounded text, mutation retry, repeated instability, and size-bound tests |
| R-05 | Traversal rejection, honest symlink classification, exact external approval | `jcode-base::startup_context::selection` | external approval, retargeting, duplicate target, and traversal tests plus editor confirmation tests |
| R-06 | Persisted receipt across snapshot, journal, startup stub, resume, reload, and split | `jcode-session-types::startup_context`; `jcode-base::session::startup_context` | receipt and preparation-block round trips; metadata-only startup-stub replay |
| R-07 | Hidden user-authority control and exact two-block file messages | `jcode-base::session::startup_context` | synthetic message structure, role, ordering, escaping, and exact-byte tests; human review for production prose |
| R-08 | Empty plans inject no Startup Context text | `jcode-base::session::startup_context` | empty install and fresh primary matrix |
| R-09 | Stable prefix, no `file_count`, timestamp exclusion, dynamic facts after files | `jcode-base::session::startup_context` | `ready_install_uses_hidden_user_messages_separate_raw_content_and_dynamic_tail`; `prepared_bundle_rebuilds_with_stable_prefix_and_locks_after_dispatch` |
| R-10 | First-dispatch freeze and truthful captured/dispatched/accepted states | Session lifecycle plus app-core preflight | rebuild, dispatch persistence, zero-provider, provider acceptance, and metadata-repair tests |
| R-11 | One server coordinator, bounded protocol, History omission | `jcode-app-core::server::startup_context`; `jcode-protocol::startup_context` | protocol suite, production protocol journey, remote History privacy |
| R-12 | One cross-process editor lease with renewal and recovery | app-core Startup Context coordinator | same-project named-coordinator contention, process guard, close, disconnect, expiry, poisoning, and fallback mechanism tests |
| R-13 | Atomic current-session and combined apply, safe busy queue, crash recovery | `jcode-app-core::server::startup_context::apply` | apply tests, persistence failure matrix, restart recovery, real busy-turn queue journey |
| R-14 | Bounded browse, search, preview, and exact receipt detail | base browser plus app-core coordinator | root/nested pages, search bounds/cancellation, current preview, Unicode detail reconstruction, stale identity rejection |
| R-15 | Fresh TUI, run, REPL, and Harness create activation; internal disablement | `jcode-app-core::agent::startup_context`; caller integration | fresh primary matrix, run output tests, direct REPL activation, Harness bridge/SDK tests, explicit internal constructors |
| R-16 | Resume/split reuse; clear/transfer recapture; reload persistence | Session and server lifecycle | receipt inheritance, resume no-recapture, clear runtime replacement, transfer child capture and cleanup |
| R-17 | Typed `Block` and `InjectDiagnostic`; caller-specific failures | base policy plus app-core activation | policy parity, direct callers, run/REPL/Harness error presentation, no production unattended selection |
| R-18 | Existing provider preflight remains authoritative | app-core preflight and provider builders | every structured provider builder, prompt-safe rollback, provider-size recovery, no automatic compaction |
| R-19 | Compact authoritative TUI state without bodies | `jcode-tui` Startup Context UI | 47-test focused family, loading/unsupported distinction, composable facets, status pagination, deterministic layouts |
| R-20 | Exact composer restoration without auto-resend | app-core blocked action plus TUI reducer | real blocked stream pair and physical composer restoration tests |
| R-21 | Complete browser, selection, ordering, preview, receipt editor | `jcode-tui::startup_context_editor` | state, transport, physical remote keys, mouse parity, preview/detail, reconnect, and render tests |
| R-22 | Equal apply actions and responsive truthful interaction | TUI editor apply and render modules | preview/apply correlation, external confirmation, per-target outcomes, hit-region and 120x30/72x24/38x12 tests |
| R-23 | Later real-turn observation, duplicate suppression, two-marker limit | base observation, Session transaction, caller turn routing | base state table, production real/synthetic/real user-turn journey, protocol origin defaults, TUI stale receipt |
| R-24 | Receipt-only default exports and warned redacted full export | `jcode-base::session::export`; app-core replay/Markdown; root CLI | raw, Markdown, replay, timeline, retained-Session, public replay default and explicit flag |
| R-25 | Native context-control participation with source immutability | context snapshot and transaction service | snapshot/detail, provider-builder range summary, apply/revert/reapply source and receipt equality |
| R-26 | Raw-body privacy and bounded observability | structural message IDs, History renderer, protocol/status/export layers | plan/recovery permissions, no-content event tests, History omission, exact detail isolation, export and debug checks |
| R-27 | Current shipped user, maintainer, recovery, and future-owner documentation | `docs/STARTUP_CONTEXT.md` and this guide | link review, source-fact review, fresh-maintainer walkthrough, package and phase-closeout review |

## Focused verification commands

Run these independently so one failure does not suppress evidence from later packages:

```bash
cargo test -p jcode-session-types --lib
cargo test -p jcode-protocol --lib
cargo test -p jcode-base startup_context --lib --no-default-features
cargo test -p jcode-app-core startup_context --lib --no-default-features -- --test-threads=1
cargo test -p jcode-tui startup_context --lib --no-default-features
cargo test -p jcode-context-core --lib
cargo test -p jcode-harness-api-server startup_context --lib
cargo test -p jcode-sdk --test client_behavior startup_context
cargo test -p jcode startup_context --lib --no-default-features
```

TypeScript SDK:

```bash
cd sdk/typescript
npm test
```

The focused families are the inner loop. They are not a substitute for the targeted
production journeys below.

## Targeted production-boundary journeys

Run each filter as a separate command or first confirm the complete module path. Cargo's
positional filter is substring matching, not a regular-expression alternation.

### Fresh caller matrix and blocked prompt transaction

```bash
cargo test -p jcode-app-core fresh_shared_primary_matrix_activates_or_blocks_before_use --lib --no-default-features -- --test-threads=1
cargo test -p jcode-app-core blocked_shared_startup_emits_prompt_safe_action_and_rolls_back_unanswered_turn --lib --no-default-features -- --test-threads=1
```

These cover empty, valid, missing, complete-source oversized, corrupt-plan, fallback, exact
issue state, zero provider dispatch, event ordering, authoritative rollback, and prompt
metadata privacy.

### Server browse, apply, queue, and later-turn observation

```bash
cargo test -p jcode-app-core startup_context_protocol_journey_uses_server_state_and_releases_editor --lib --no-default-features -- --test-threads=1
cargo test -p jcode-app-core busy_startup_apply_drains_after_active_turn_before_next_user_prompt --lib --no-default-features -- --test-threads=1
cargo test -p jcode-app-core later_real_user_turn_observes_staleness_before_prompt_and_continues --lib --no-default-features -- --test-threads=1
```

These use the production shared-server and provider request path, not only reducer helpers.

### Cross-process ownership and restart recovery

```bash
cargo test -p jcode-app-core same_project_is_exclusive_across_sessions_and_named_coordinators --lib --no-default-features -- --test-threads=1
cargo test -p jcode-app-core process_termination_releases_the_kernel_held_project_guard --lib --no-default-features -- --test-threads=1
cargo test -p jcode-app-core restart_recovery_converges_from_queued_prepared_plan_and_session_commit_stages --lib --no-default-features -- --test-threads=1
```

Unix is the runtime-verified lock path. The non-Unix exclusive-owner-file fallback is
mechanism-validated unless run on a supported target.

### Clear, transfer, resume, split, run, and Harness

```bash
cargo test -p jcode-app-core handle_clear_session_replaces_runtime_handles_and_updates_shutdown_registration --lib --no-default-features -- --test-threads=1
cargo test -p jcode-app-core transfer_child_uses_authoritative_handoff_instead_of_invalid_legacy_compaction --lib --no-default-features -- --test-threads=1
cargo test -p jcode-base receipt_round_trips_snapshot_journal_stub_remote_resume_and_split --lib --no-default-features
cargo test -p jcode restore_agent_session_if_requested_restores_resumed_session --lib --no-default-features
cargo test -p jcode run_startup_context_failure_preserves_plain_json_and_ndjson_contracts --lib --no-default-features
cargo test -p jcode-harness-api-server startup_context_failure_answers_create_with_typed_error --lib
cargo test -p jcode-harness-api-server missing_attach_is_rejected_without_fresh_session_fallback --lib
cargo test -p jcode-sdk --test client_behavior create_session_preserves_typed_startup_context_failure
```

### Provider framing, export, History, detail, and context control

```bash
cargo test -p jcode-context-core synthetic_startup_messages_are_accepted_by_every_primary_structured_provider_builder --lib
cargo test -p jcode-context-core explicit_range_summary_can_include_startup_messages_without_mutating_source --lib
cargo test -p jcode-app-core startup_context_summary_revert_and_reapply_preserve_source_receipt_and_cache_semantics --lib --no-default-features
cargo test -p jcode-app-core startup_context_markdown_export_is_receipt_only_unless_contents_are_explicitly_requested --lib --no-default-features
cargo test -p jcode-app-core startup_context_replay_is_receipt_only_by_default_and_full_only_by_explicit_policy --lib --no-default-features
cargo test -p jcode-tui remote_optimistic_history_omits_receipt_owned_startup_and_stale_bodies --lib --no-default-features
```

## Broad suites

The complete app-core suite is environment-sensitive and must run serially:

```bash
cargo test -p jcode-app-core --lib --no-default-features -- --test-threads=1
```

Other relevant complete suites:

```bash
cargo test -p jcode-session-types --lib
cargo test -p jcode-protocol --lib
cargo test -p jcode-context-core --lib
cargo test -p jcode-harness-api-server --lib
cargo test -p jcode-sdk --tests
```

The monolithic TUI library suite contains long signal, smoothness, onboarding, and startup
tails. It is optional evidence, not a substitute for the complete focused Startup Context
family, neighboring remote-input and context-control families, deterministic terminal-cell
layouts, strict lint, and the activated workflow. If deliberately run, report an
interruption or timing failure honestly.

## Phase integration matrix

The final combined source must cover these journeys:

| # | Journey | Automated evidence | Hands-on supplement |
|---|---|---|---|
| 1 | Empty project, first prompt, resume, export | Fresh primary matrix, empty install, resume and export projection tests | Confirm muted `none` and no forced editor |
| 2 | Valid ordered files, first prompt, accepted receipt, resume, split | Session install/delivery tests, recording provider, receipt persistence/split | Confirm ordered receipt and accepted state |
| 3 | Git subdirectory plus second worktree | Base real-Git identity and active-root capture tests | Optional when a convenient worktree exists |
| 4 | Missing initial file, preserved composer, editor repair, successful dispatch | Blocked stream pair and TUI exact restoration | Required review of the real resolution workflow |
| 5 | External confirmation and retargeted symlink | Base selection security and TUI external-confirmation tests | Required review of exact-target confirmation |
| 6 | Safe provider budget block, selection/model repair, manual resubmit | Existing production preflight and TUI prompt-preservation families | Required review when a practical model/project fixture exists |
| 7 | Busy turn, queued late batch, later accepted request | Production busy-turn queue test | Required review of visible queued and final per-target state |
| 8 | Changed file, one marker, continued turn, duplicate suppression, limit | Base state table and real-user turn integration | Required review of stale receipt and continued workflow |
| 9 | Default export omission and explicit full export | Raw, Markdown, replay tests plus activated CLI smoke | Required review of output and stderr warning |
| 10 | Context summary, revert, reapply | Context snapshot, provider validation, transaction service | Required review through `/context edit` |
| 11 | Run, REPL, Harness create, attach, and failures | Root, Agent, bridge, Rust SDK, TypeScript SDK | Public smoke only where cheaper than existing production-path tests |
| 12 | Clear and transfer recapture; resume and split reuse | Clear, transfer, resume, split tests | Confirm ordinary lifecycle labels if touched in review |
| 13 | Reload during lease and apply recovery | Exec-lock, process-exit, restart-recovery, reload suites | Confirm editor can reopen after final activated reload |
| 14 | Wide, narrow, extreme-narrow mouse and keyboard | 120x30, 72x24, 38x12 cells, physical remote key tests, hit regions | Required visual and workflow review by Mirza |

## Activated export smoke

A strong public acceptance route needs no provider credentials. Create an isolated scratch
Session containing distinct synthetic sentinels for control prose, file content,
stale-marker prose, and a secret-shaped value. Then run the activated binary:

```bash
~/.jcode/builds/current/jcode replay <scratch-session> --export \
  >default.json 2>default.stderr

~/.jcode/builds/current/jcode replay <scratch-session> --export \
  --include-startup-context >full.json 2>full.stderr
```

Verify:

- Default stdout omits every body sentinel.
- Default stdout retains path, resolved path, digest, bytes, ordinal, batch kind, and
  delivery state.
- Default stderr has no full-content warning.
- Explicit stdout includes every Startup Context body.
- The secret-shaped value is replaced by `[REDACTED_SECRET]`.
- Explicit stderr contains the privacy warning.
- Both stdout files remain valid JSON.
- The source Session is unchanged.
- Every scratch Session and output is removed after inspection.

## Debug and TUI verification

Run the complete focused TUI family first. It covers every compact state and meaningful
editor/apply layer at 120x30, 72x24, and 38x12.

After final build-reload, use the debug socket against the activated client:

```text
startup-context-fixtures
startup-context-fixture:<name>
startup-context-state
```

The in-TUI route is:

```text
/debug-fixture startup-context <state>
```

Representative fixture coverage includes loading, none, ready, blocked, blocked action,
dispatched, accepted, queued, stale, busy, unsupported, metadata repair, storage error,
populated editor, invalid draft, external confirmation, apply review, applying, recovery,
partial outcome, success, failure, and cancellation.

Debug status must remain metadata-only. Raw receipt content may appear only through an
explicit preview or exact detail action.

Headless frame capture has historically returned no frames or delayed responses even when
the activated UI is correct. Do not make that one transport the only visual acceptance
path. Pair deterministic terminal cells with Mirza's visible activated-client review.

## Reload-sensitive ordering

Run suites that intentionally write synthetic canary or pending-activation state before
final activation:

```bash
cargo test -p jcode-app-core reload --lib --no-default-features -- --test-threads=1
cargo test -p jcode-tui reload --lib --no-default-features
cargo test -p jcode reload --lib --no-default-features
```

Then perform the final coordinated activation:

```text
selfdev build-reload target=tui
selfdev status
```

Verify:

- The running version contains the final committed HEAD.
- Current and shared-server channels match.
- Socket state is ready.
- The matching canary passed.
- No reload-state-mutating suite runs afterward.

A missing build-reload tool response does not prove failure. Inspect `selfdev status`
before rebuilding. Server replacement can interrupt the response after the exact new
binary is already active.

## Build, lint, and repository gates

Run formatting and diff checks on the final source:

```bash
cargo fmt --all -- --check
git diff --check
git diff --check b168305144fa7da49e9411257076696f05d08ddc..HEAD
```

Strict Clippy for affected targets:

```bash
cargo clippy -p jcode-session-types --all-targets -- -D warnings
cargo clippy -p jcode-protocol --all-targets -- -D warnings
cargo clippy -p jcode-base --lib --tests --no-default-features -- -D warnings
cargo clippy -p jcode-context-core --all-targets -- -D warnings
cargo clippy -p jcode-app-core --lib --tests --no-default-features -- -D warnings
cargo clippy -p jcode-tui --lib --tests --no-default-features -- -D warnings
cargo clippy -p jcode-harness-api-server --all-targets -- -D warnings
cargo clippy -p jcode-sdk --all-targets -- -D warnings
cargo clippy -p jcode --all-targets --no-default-features -- -D warnings
```

Also compile the root public surface:

```bash
cargo check -p jcode --all-targets --no-default-features
```

Before broad Rust suites, check disk headroom. If necessary, remove only disposable
`target/debug` artifacts. Preserve source, commits, `target/selfdev`, the activated binary,
durable state, and protected paths.

## Documentation checks

Review these current-behavior documents together:

- [`../STARTUP_CONTEXT.md`](../STARTUP_CONTEXT.md)
- [`../CONTEXT_CONTROL.md`](../CONTEXT_CONTROL.md)
- [`../README.md`](../README.md)
- Root [`../../README.md`](../../README.md)
- TypeScript SDK [`../../sdk/typescript/README.md`](../../sdk/typescript/README.md)

Verify every relative link resolves and every user-visible command matches current CLI or
TUI help. Source facts must match current implementation, especially:

- Project identity and worktree behavior.
- 1,024-entry and 32 MiB complete-source bounds.
- `captured`, `dispatched`, and `provider accepted` distinctions.
- Resume/split reuse and clear/transfer recapture.
- Receipt-only default export and `--include-startup-context` warning on stderr.
- `/context` use of authoritative messages.
- Structural message identity instead of XML parsing.
- Phase 3, 4, 9, and 10 ownership handoffs.

Do not add deterministic tests that judge the approved agent-facing prose.

## Privacy review

Use separate synthetic sentinels for:

- Startup control prose.
- Exact file body.
- Late-update control prose.
- Stale-marker prose.
- Secret-shaped content.

Check each retained representation, not only what the TUI draws:

- Project plan JSON.
- Apply transaction and terminal record.
- Session receipt metadata.
- Live and persisted remote History.
- Optimistic remote startup state.
- Compact status and paginated receipt events.
- Debug state.
- Ordinary diagnostics and lifecycle logs.
- Default raw, Markdown, replay, timeline, interactive, and video exports.
- Explicit exact detail and full-content export.
- Authoritative `/context` snapshot and detail.

The intended boundary is proportional: useful paths, resolved targets, digests, sizes,
ordinals, batch kinds, observations, issues, and delivery state remain visible. Raw bodies
stay behind explicit detail or warned full-export routes.

## Manual acceptance by Mirza

Automated tests verify mechanisms. Mirza's review remains the acceptance route for:

- Initial, late-update, and stale-marker prose.
- Compact strip and editor visual quality.
- Physical keyboard and mouse usability.
- Exact external-target confirmation.
- Blocked composer preservation and recovery clarity.
- Queued and partial apply truthfulness.
- Stale receipt guidance.
- Default and full-context export clarity.
- `/context` summary, revert, and reapply workflow.

Record the activated runtime identity, terminal sizes, project fixture, reviewed journeys,
and any limitation. Repair any in-scope defect, rerun the affected automated path, rebuild,
and return the candidate for review before claiming package completion.

## Acceptance boundaries

- No live provider spending is required when production request builders, recording
  providers, and activated UI or CLI routes represent the behavior.
- Unix cross-process locking is runtime-verified on Unix. The non-Unix fallback is
  mechanism-validated unless that platform is available.
- The complete TUI monolithic suite is optional because its long unrelated tail is not a
  stronger substitute for acceptance-aligned focused paths.
- Final package approval does not itself mark the phase complete. A separate phase-closeout
  session reconciles the complete source, updates the capability map and roadmap, and
  obtains final phase acceptance.
