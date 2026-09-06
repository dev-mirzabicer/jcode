# WP-07 workflow migration evidence

**Status:** Candidate verification in progress. This is technical evidence, not work-package acceptance.

**Starting revision:** `99bba69497a04fa48fac675c5e40d05e39a929ca`.

## Scope and disposition

The inventory closes 51 eligible WP-07 rows: WFL-001 through WFL-046, SYS-004/005, LEG-005/006, and TGD-001. WFL-045 and WFL-046 were discovered by tracing the existing ambient initial-turn and takeover delivery sinks. SYS-006 remains code-owned system-reminder framing. No context-curator, dormant memory, provider-required identity, executable tool schema, or self-development mechanic was migrated.

| Inventory family | Final owner and representative evidence |
|---|---|
| Mission, WFL-001 | Managed introduction, continuation and default intent. Owner retains XML and literal escaped user data. Mission and TUI tests cover unchanged prior state/input on render failure. `WP07_MISSION_EQUALITY.json`. |
| Commands, WFL-002–016 | Shared managed command composer. Local callers render before mutation. Remote callers send typed preparation to the server and retain existing send/soft-interrupt/cancel/queue policies. Real transport, daemon and TUI tests cover failure, cancellation, correlation and reload. `WP07_COMMAND_EQUALITY.json`. |
| Review/judge, WFL-017–020 | Managed startup and shared guardrail/context modules. Server captures the parent and renders before cloning. Typed startup metadata replaces prose inference. Real split failure/retry and TUI restoration tests. `WP07_REVIEW_EQUALITY.json`. |
| Transfer, WFL-021/022 | Managed true-system and task prose. Recording provider verifies both exact parts, full-input budgeting, unchanged transcript formatting, no-call failures and whole instructions. `WP07_TRANSFER_EQUALITY.json`. |
| SDK structured output, WFL-023/024 | Both SDKs use the connected session server's managed renderer. Local schema validation and retries stay SDK-owned. Rust/TypeScript API, bridge and daemon tests. `WP07_STRUCTURED_EQUALITY.json`. |
| Ambient, WFL-025/045 | Managed global cycle prose. Facts and dormant memory stay owner-controlled. Selected prose renders before consuming directives or launching. Synthetic and corrected root E2E builder tests. `WP07_AMBIENT_EQUALITY.json`. |
| Overnight, WFL-026–033 | Existing supervisor retains timing, policy and artifacts. Typed phase identity replaces prompt-prefix parsing. Render-before-publication/phase-consumption and TUI poke recovery tests. `WP07_OVERNIGHT_EQUALITY.json`, `WP07_OVERNIGHT_POKE_EQUALITY.json`. |
| Swarm tasks, WFL-034–044/046 | Managed planner, integration, worker contracts, restart and takeover prose. Structural framing remains in Swarm core; plan mutation/delivery remain server-owned. Worker scope, failed-assignment rollback, identical explicit retry after source repair, and failed stand-down preventing takeover have production-handler tests. `WP07_SWARM_WORKER_EQUALITY.json`, `WP07_SWARM_CONTROL_EQUALITY.json`. |
| Effort, SYS-004/005 | Existing selection and model-effort mapping are unchanged. Scoped managed dynamic suffix and fail-before-provider handling. One request snapshot supplies local accounting and dispatch. `WP07_EFFORT_EQUALITY.json`. |
| Preferred tools, LEG-005/006 | Receipt-gated global cutover and optional project compatibility through the shared runtime. Existing caller ordering preserved, explicit project-only scope cannot duplicate global. Real-Git import/empty/shadow/failure tests. `WP07_PREFERRED_EQUALITY.json`. |
| Routing, TGD-001 | One optional session scalar stores the complete validated Swarm description after successful preflight. No process-wide prose cache. Real provider, snapshot/journal/remote-startup/split, source-failure and editor tests. `WP07_ROUTING_EQUALITY.json`. |

The 13 JSON artifacts contain 354 recorded migration comparisons, 351 exact and three explicitly identified data corrections. These are one-time records, not permanent prose tests. The transfer artifact records the prose-migration comparison before the separately approved complete-input accounting correction. Its bounded conversation excerpt is not claimed byte-identical under the final corrected budget. The routing prose remains exact; its code-owned editor pointer changed intentionally.

The historical full-retry helper branch in WFL-041 is still identified as an undelivered compatibility branch, not claimed as a current task-control execution path. Its full data rendering remains covered separately from the actual shorter retry path.

## Decisions and necessary repairs

- Mirza approved server-side structured rendering and TypeScript parity, retaining SDK-owned validation/retry behavior.
- Mirza approved transfer accounting for complete system and user input. Instructions remain whole; only the already-bounded conversation excerpt may shrink or preparation may block.
- A TypeScript truncation edge could split a UTF-16 surrogate and produce JSON rejected by Rust transport. The existing limit/marker remain; the cut now respects surrogate pairs.
- Mission's chained replacement could reinterpret a placeholder inside user objective data. Data now remains literal and XML-escaped. Managed prose is unchanged.
- Mirza authorized the minimal routing refinement on 2026-09-06 at 12:41 UTC. One session string replaces cross-project process-wide sharing. Existing dispatched sessions attribute one deliberate cache/continuation transition on first capture. Swarm remains enabled and unchanged otherwise; Phase 4 owns its future.
- A real-Git regression proved a later seed upgrade recreated a previously committed deletion. Historical-path detection now preserves that deletion. The existing no-resurrection rule, rather than a new policy, governs this repair.
- The root ambient builder test had a pre-existing obsolete argument shape. It now exercises the current API with synthetic managed prose. Remaining touched mutable-prose assertions were replaced with synthetic mechanism tests.

## Combined verification

Reproduction script and individual logs: `/Users/mirzabicer/.jcode/scratch/wp07/final-verification.sh` and `final-verification/`.

The combined run passed instruction/runtime/store, prompt compatibility, transfer, Startup Context, origin, primary lifecycle, routing, worker/control, ambient, overnight, daemon workflow, mission, late-MCP/tool stability, replay, review/judge/TUI failure, protocol, context-core, pure workflow, SDK and Harness checks. Root all-target compilation and strict affected-crate/root library+binary Clippy passed.

The broad TUI command group reported 203 passed and two pre-existing disabled-memory goal failures. They were not weakened or represented as passing. An initially incorrect spawn test filter matched zero and was rejected by the wrapper; the correctly named `comm_session_tests` rerun passed, along with the final deep-dispatch and prompt-composition test cleanup. No zero-test filtered invocation counts as evidence.

Verification is on macOS arm64. Model requests in tests use recording providers or local fixtures. No paid model evaluation, actual swarm delegation, ambient enabling, or desktop change is part of this work.

## Activation

Final coordinated build/reload, binary/channel/canary identity and activated public journeys are recorded here after they run. The live instruction store is not treated as upgraded merely because test stores adopted seed 26.

## Future owners

WP-09/WP-10 consume the final resource/consumer catalog, high-impact scoped redefinitions, typed metadata and existing Git service. The central manager remains their work. Phase 4 owns isolated delegation and any Swarm disabling/replacement. Phase 5 owns primary role prose, Phase 6 skills, Phase 7 additional tool guidance, and Phase 9 async policy. No existing workflow silently adopted the model roster or a new primary profile.

### Verification tail

Coordinated task `482976kh39` completed the combined matrix, including successful root all-target check, corrected root ambient E2E, affected-crate strict lint and root library/binary strict lint. Task `811104ijwa` passed the corrected spawn filter and final synthetic-test cleanup. TypeScript `npm run check` passed all 47 tests. Raw logs preserve the known failures and every zero-match correction rather than relabeling them as successful.

### Activated integration correction

The first activated TUI command journey exposed a real wake-up gap: typed command preparation was queued without requesting the idle event-loop dispatcher. Command creation, render replies and reconnect now request the existing dispatch wake-up. The main event loop and regression test share its flag-clearing boundary, and waiting for a reply does not spin. Task `838766nuc5` passed actual Enter/reply dispatch, command recovery, adjacent follow-up tests and strict TUI lint. The same activated journey must pass after rebuilding this correction.
