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
