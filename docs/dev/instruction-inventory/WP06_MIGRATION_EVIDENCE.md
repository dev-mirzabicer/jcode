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
