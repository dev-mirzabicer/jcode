# WP-06 notification migration evidence

**Status:** Implementation in progress. This is a migration ledger, not work-package acceptance.

**Starting source:** `c3421a486bde8ea332f8c64696adb1db31a86f6f`, tree `3be2aa9161f01ad2a970410b3986f467d9663862`.

## Recovery, fork and transfer

Rows `INS-NTF-002`, `INS-NTF-003`, and `INS-NTF-008` through `INS-NTF-012` now use typed occurrence-time notification resources. Session and Agent owners retain XML/bracket framing, roles, display roles, ordering, trigger conditions and retry limits. Transfer keeps its heading, source identity and complete summary outside editable prose.

The one-time probe compared the original production format expressions with the managed production renderer using the same values. For fork and transfer it inspected the actual appended `ContentBlock::Text`, not the whitespace-normalized display preview. Recovery comparisons reconstructed the unchanged owner wrappers around the production managed render. Separate Agent tests exercise real recovery delivery and trigger limits.

[`WP06_FIRST_FAMILY_EQUALITY.json`](WP06_FIRST_FAMILY_EQUALITY.json) records 14 passing byte comparisons for nine resources, including all three guardrail stages and six stop-reason inputs (ordinary, whitespace, markup and empty). The original templates were captured before their source migration. No prose exception was taken. The disposable test was removed after its successful run. It is not a recurring prompt-quality or phrase test.

Synthetic permanent tests cover current-source rereads, earlier result preservation, typed substitutions without HTML escaping, project redefinition, intentional empty body, invalid shadowing, deletion revealing global, missing singleton failure, unrelated invalid-resource isolation, and actual fork delivery without mutation on render failure.

The broader WP-06 inventory remains open until its remaining families and final integration checks are complete.

## Startup Context

Rows `INS-NTF-004` through `INS-NTF-006` now render managed initial, late-update and state-specific stale prose. The common stale remainder is one managed module. The session owner still supplies the exact XML tags and attributes, escaping, timestamps, roles, IDs, file order, receipts and two-notice limit.

Control rendering is lazy inside the existing batch builders. Empty plans and receipt-only late updates never read notification sources. Prepared late applies retain their complete rendered message through persistence/recovery instead of rendering again at apply. A failed render returns a typed instruction error before installation, preparation or observation publication. Observation failure restores the prior complete session, including earlier file observations in that transaction.

[`WP06_STARTUP_EQUALITY.json`](WP06_STARTUP_EQUALITY.json) records six passing one-time comparisons using the real owner formatters: initial, late and all four active stale states. Attribute fixtures include XML metacharacters. The diagnostic `Current` arm remains unreachable for real stale notifications and retains its existing debug assertion. No prose exception was taken. The disposable equality test was removed.

The final run for this slice passed 55 permanent Startup Context tests plus the disposable equality test. New production-path synthetic journeys cover later-occurrence freshness, exact earlier messages, receipt IDs and counts, unchanged roles/timestamps/order, empty content, invalid-content rollback, lazy no-event behavior, and prepared late-text persistence after source edits. Strict library Clippy passed for base, app-core and TUI.

## Server wake reminders

Rows `INS-NTF-014`, `INS-NTF-015` and the instruction portion of `INS-NTF-037` use managed background-completion, Swarm-await and DM/channel/broadcast wake guidance. Existing peer envelopes and raw peer text remain delivery-owned runtime data. Notification fanout, busy-session soft queues and wake selection remain unchanged.

The tracked-turn owner resolves a managed reminder using the recipient Agent's working directory only after the wake path selects that live session. An invalid reminder produces the existing terminal Error event and failed status without a provider call or transcript append. It is not converted into the busy-session fallback queue. Already-rendered reload recovery text remains with its excluded owner.

[`WP06_WAKE_EQUALITY.json`](WP06_WAKE_EQUALITY.json) records five exact comparisons, including channel/sender substitution without escaping. The one-time probe was removed. A real tracked-turn test with a recording provider verifies invalid project shadowing, zero provider calls on failure, current project prose on the next occurrence, and no rendering/dispatch while busy. Existing wake and background delivery suites pass. Strict all-target Clippy passed for base, app-core and TUI.

## Tool-operation notices

Rows `INS-NTF-026`, `INS-NTF-034` and `INS-NTF-035` use managed config-edit guidance, command refusal prose and macOS permission guidance. ToolContext supplies project specificity. Configuration keys/values/liveness rows and permission status fields remain code-owned. The existing human-only configuration summary keeps its built-in copy; model-facing notices no longer use that prose path.

The workspace Rust `GateOutcome` now carries only Allow, Reflect or Deny. The risk crate retains classification and justification policy without depending on the instruction runtime. The Bash adapter renders the selected refusal and never turns empty, invalid or missing prose into authorization. Its real tool-path regression uses only a disposable output target and verifies the command never executes across all four source states. All 67 risk tests pass. No tool schema, risk threshold or permission rule changed.

A config file mutation can succeed while its notification fails. That result retains the successful write/diff and adds a distinct rendering-error diagnostic instead of pretending to roll back the file. The real write and apply-patch paths retain cache invalidation and liveness behavior. Missing macOS permissions still trigger guidance; ready status does not read its source. Managed permission prose bypasses the general 16,000-character computer-data cap, with a complete 200,000-character synthetic test. Other computer actions keep their existing cap.

[`WP06_TOOL_NOTICES_EQUALITY.json`](WP06_TOOL_NOTICES_EQUALITY.json) records 14 exact full-output comparisons: eight permission-state combinations, live/restart/mixed/invalid config results, and both command-refusal branches. All three disposable probes were removed. Focused failure-path and config suites pass. The computer suite passes its 12 non-GUI tests and retains 10 explicitly ignored live-GUI tests. Strict all-target Clippy passes for base, app-core, TUI and command-risk. No live desktop action was performed for this migration.

## Readiness, reload and scheduled notices

Rows `INS-NTF-013`, `INS-NTF-027` and `INS-NTF-028` use managed scheduled-task announcements, browser status/readiness prose and interrupted-wait guidance. Browser state, metadata and action blocking remain code-owned. Ready actions do not read a blocking-notice resource. The existing fake-bridge integration test still rejects a stale setup marker, now using synthetic managed guidance.

Reload completion retains its tool-result wrapper, input serialization, duration formatting and error flag. It must complete the pending tool receipt even if instruction rendering fails, so that distinct failure is reported in the result rather than dropping the receipt. The selfdev-specific branch and non-wait factual result remain unchanged and do not read managed source.

Scheduled announcements use the task's captured working directory. The live-to-headless fallback reuses the same rendered message. Failed rendering occurs before a spawned execution is published. The scheduler's existing dequeue-and-log failure policy is unchanged; this package does not add retries or redesign unattended execution. Phase 9 remains that policy's owner.

[`WP06_READINESS_SCHEDULE_EQUALITY.json`](WP06_READINESS_SCHEDULE_EQUALITY.json) records 44 exact comparisons: nine browser outputs, three wait-like reload outputs and all 32 combinations of optional scheduled-message fields. The probes were removed. Browser, reload and real scheduled-child tests pass, including mutable prose, unchanged state/metadata, fail-closed browser actions, explicit reload diagnostics and no child on scheduled-render failure. Strict all-target Clippy passes for base, app-core and TUI.

## Generated images and background result records

Rows `INS-NTF-007` and `INS-NTF-033` use managed image explanation and empty-output prose. Image pixels, the 20 MiB attachment policy, supported-format checks, metadata/revised-prompt framing and caller image-support gating remain unchanged. A rendering failure is explicit while the completed image's pixels and metadata remain available.

Background record headers, status fields, preview/failure data, retrieval commands and their parser remain code-owned. Only the separable empty-output sentence moves. Local delivery now reuses its one rendered occurrence for display, provider context and persistence rather than formatting twice. Server delivery uses recipient working-directory metadata and does no rendering when both notify and wake are disabled.

[`WP06_IMAGE_BACKGROUND_EQUALITY.json`](WP06_IMAGE_BACKGROUND_EQUALITY.json) records ten exact comparisons, including nine image metadata/revised-prompt combinations and the complete empty background record. The probe was removed. Base message tests, real source-update/empty/failure tests, complete Harness bridge tests and background regressions pass. Strict all-target Clippy passes for base, app-core, TUI and Harness bridge.

## Background tool guidance

Rows `INS-NTF-029` and `INS-NTF-030` now use managed foreground promotion, detached/reload continuation, background launch/delivery/progress guidance, task-selection hints, timeout followup and full-output retrieval guidance. The background result formatter owns task IDs, paths, durations, structural records and explicit rendering-failure diagnostics. A prose error cannot lose an already-started task receipt or change task execution, cancellation, delivery or wait policy.

[`WP06_BACKGROUND_TOOLS_EQUALITY.json`](WP06_BACKGROUND_TOOLS_EQUALITY.json) records 14 comparisons covering complete launch/promotion/reload records, all delivery combinations, timeout guidance and background selection/output prose. The one-time probes were removed. Existing Bash and bg suites pass, as do synthetic current-source/empty/failure tests and a 200,000-character instruction suffix beyond the unchanged 50,000-byte data preview limit. Strict all-target Clippy passes for base, app-core and TUI.

## Coordination announcements

Rows `INS-NTF-016`, `INS-NTF-017`, `INS-NTF-036` and `INS-NTF-038` now use managed coordinator, plan lifecycle and file-activity prose. Each recipient's server-owned working directory supplies specificity. Elections, cleanup, plan mutations, notification types, queue source, sender identities, file-conflict metadata, summary/intent suffixes and delivery order remain with the server. A rendering failure is explicit rather than undoing already-completed server state changes.

[`WP06_COORDINATION_EQUALITY.json`](WP06_COORDINATION_EQUALITY.json) records 16 comparisons across all migrated announcements, rejection reason presence and both file-activity directions with all summary/intent combinations. The probe was removed. Plan, coordinator, file-activity and synthetic source-update/failure tests pass. Former coordinator/plan prose assertions use synthetic resources. Strict all-target Clippy passes for base, app-core and TUI.

## Integration selection and plan-driver recovery

Rows `INS-NTF-031` and `INS-NTF-032` now use managed integration guidance and plan-driver continuation/recovery prose. Validated catalog receipts, provider/setup data, IDs, error causes, failure counts, login-route selection, task state, driver limits and cleanup policies remain code-owned. Instruction rendering failures are explicit without discarding an already-recorded selection or started driver receipt.

Retention-hint idempotency now uses a typed in-process error marker rather than searching error prose for `swarm cleanup`. This allows arbitrary edited guidance and stops incidental error data from suppressing the hint. It does not change driver retry or worker-retention policy. A previously untyped error containing that phrase now correctly receives guidance, which is a narrow recognition correction, not a seed wording change.

`WP06_DISCOVERY_EQUALITY.json` compares eleven full production outputs against the exact pre-migration production functions. `WP06_DRIVER_EQUALITY.json` compares sixteen driver outputs/sections across provider routes, terminal states and recovery branches. Both probes were removed. Discovery's complete suite, local HTTP receipt tests, credential-wave tests, structural retention tests and terminal-summary tests pass. Strict all-target Clippy passes for base, app-core and TUI. Permanent managed-prose tests use synthetic sources.

## Approved scope and UI clarification

Mirza authorized the recommended corrections on 2026-09-05. `INS-NTF-001` remains a code-owned environment-fact record. Todo controls will receive structural queue/history identity and server-owned remote source rendering. The reproduced mixed-queue UI defect must retain the user's text without changing combined model-visible bytes. These clarifications do not constitute work-package acceptance.
