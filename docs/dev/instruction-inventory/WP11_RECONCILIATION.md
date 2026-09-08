# Final instruction migration reconciliation

**Reconciled against:** the combined Phase 3 source through `e8035d94b`.
**Boundary:** migration accounting, not WP-11 or Phase 3 acceptance.

## Inventory closure

The authoritative inventory contains 129 unique rows. Every row has a named
owner and final disposition. The original 127-row baseline was extended by
WFL-045 and WFL-046 after production-sink tracing. NTF-001 and SYS-006 retain
explicit factual/structural dispositions alongside the original 28 exclusions.
The remaining 99 rows describe managed or deliberately retained source inputs,
including dedicated AGENTS.md and project compatibility sources.

| Rows | Final source/delivery ownership | Preservation evidence |
|---|---|---|
| SYS-001, SYS-002 | Managed compatibility body and Mermaid. Embedded constants are seed input only, not a second active reader. | WP03 evidence and `WP11_CORE_EQUALITY.json`: exact production renders, 1,558 and 49 bytes |
| LEG-001 through LEG-004 | Composer/repository receipt-gated system/common cutover. Global originals remain inactive. Project compatibility remains only under the defined absence/conflict rules. | WP02/WP03 import evidence plus `WP11_COMPATIBILITY_EQUALITY.json` and synthetic missing-import tests |
| EXT-001, EXT-002 | Dedicated runtime-captured ecosystem input, not automatic import. Primary scope order is global first; unprofiled compatibility retains project first. | Original captures, composition tests, and WP11 full/split equality |
| LEG-005, LEG-006 | Paired managed preferred-tool resources and receipt-gated project compatibility. | `WP07_PREFERRED_EQUALITY.json` and WP11 combined equality |
| SYS-003, SKL-001, SKL-002 | Managed catalog prose, one managed/external skill source model, complete package Copy and exact active invocation snapshots. | WP05 evidence, skill/package tests and WP11 combined lifecycle |
| TGD-001 | Session-owned complete validated Swarm description, captured after successful preflight. Constructor schema text is separate. | `WP07_ROUTING_EQUALITY.json`, production routing preview/capture tests |
| SYS-004, SYS-005 | Managed request-time effort suffixes. Existing effort selection and provider mapping remain owner-controlled. | `WP07_EFFORT_EQUALITY.json` and recording-provider tests |
| SYS-006 | Code-owned System Reminder heading and delivery boundary. | Final source inspection and original structural exclusion |
| NTF-001 | Generated environment facts and structural identity, explicitly excluded under the WP06 decision. | Accepted WP06 decision and current `build_session_context` owner |
| NTF-002 through NTF-038 | Typed occurrence-time notification resources. Session, tool, task, Startup Context and server owners retain delivery and state transitions. | Eleven WP06 equality artifacts, 407 exact comparisons, final notification/queue/wake tests |
| WFL-001 through WFL-046 | Typed managed workflow modules/system/notification resources. Existing workflow owners retain execution, framing, model and permission policy. | Thirteen WP07 artifacts, 354 cases, named exceptions below, final owner/SDK tests |
| EXC-001 through EXC-028 | Context curator, dormant memory, selfdev mechanics, provider protocol/identity, schemas, structural/runtime data, diagnostics/tests, desktop and historical recognition remain with their explicit owners. | Original exclusion evidence and final source/range review |

The final pass traced the remaining production compatibility builder, not just
managed primary activation. Commit `6191ca198` removed its duplicate raw base,
overlay and available-skills paths and the unused ecosystem loader. Full and
split public builders now delegate complete composition to the instruction
runtime's composer. There is no active fallback to embedded prose after store
cutover. A missing imported common destination now blocks its affected use
rather than silently disappearing.

Unprofiled compatibility callers deliberately retain their existing lifecycle
and ordering. They do not acquire the primary kernel, addenda, defaults,
availability policy or frozen profile state merely through this migration.
Canonical project source identity is shared with the framework, including from
nested working directories. The delegated WP11 decision and current workflow
guide record this boundary. It does not authorize another primary role set,
Swarm disabling, roster adoption or asynchronous job implementation.

## One-time equality, not prose tests

- WP06: 407 exact cases across 11 artifacts.
- WP07: 354 cases across 13 artifacts, 351 exact and three named data-integrity
  corrections. The original transfer artifact predates its separately approved
  complete-input budget correction.
- WP11 core: exact compatibility and Mermaid production renders against the
  original captures, with no provider request.
- WP11 compatibility: 24 complete full/static/dynamic outputs across eight
  combinations of synthetic legacy inputs, empty ecosystem content, capability
  selection and dynamic input. Before and after outputs are byte-identical.
  The artifact includes the fixture contract and hashes. The disposable Rust
  capture test was removed immediately afterward.

Permanent tests use synthetic managed content for discovery, scope, rendering,
editing, failures, persistence and cache behavior. No deterministic test grades
or freezes central prompt or skill wording.

## Explicit differences and protected boundaries

These are named differences, not missing equality evidence:

1. The approved profile kernel and transition/replacement sentences are new
   reviewed framework prose. Primary paired scopes intentionally became
   global-before-project. Their current text was reviewed again by Mirza during
   WP11 on 2026-09-08.
2. WP07 corrected literal mission data substitution and two TypeScript surrogate
   boundaries. These are the three non-equal data cases in the equality artifacts,
   not unreviewed prompt rewrites.
3. Transfer now budgets complete system and user instructions together. Its
   already-bounded conversation excerpt may shorten or block near capacity.
4. Routing prose remains exact while its code-owned editor pointer and explicit
   session snapshot/cache-transition lifecycle changed under the accepted WP07
   decision.
5. WP11 completes managed source authority for previously missed compatibility
   consumers. Their canonical project lookup, invalid-source failures and
   required imported-common behavior are documented explicitly.
6. The full phase whitespace check reports six extracted workflow files whose
   literal trailing/newline bytes preserve the previous runtime output. Do not
   trim those files merely to make a source-format check pass:

| Preserved file under `instruction/workflow/` | Inventory | Equality artifact |
|---|---|---|
| `ambient-directives.md`, `ambient-identity.md` | WFL-025/045 | `WP07_AMBIENT_EQUALITY.json` |
| `judge-visible-context.md` | WFL-018 | `WP07_REVIEW_EQUALITY.json` |
| `review-read-only-guardrails.md` | WFL-017 | `WP07_REVIEW_EQUALITY.json` |
| `mission-introduction.md` | WFL-001 | `WP07_MISSION_EQUALITY.json` |
| `overnight-poke-intro.md` | WFL-033 | `WP07_OVERNIGHT_POKE_EQUALITY.json` |

The working/WP11 range checks are separate from that historical byte-fidelity
boundary. Context-core and curator changes in the full Phase 3 range are fixture
`origin: None` additions, not changes to protected curator prose or projection
architecture. Memory remains hard-disabled. Startup Context remains authoritative
user-message source with its existing receipts and export policy.
