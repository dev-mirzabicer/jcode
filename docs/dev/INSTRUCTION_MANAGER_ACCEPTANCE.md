# Instruction manager verification record

This records Phase 3 WP-09 implementation evidence. It is **not** user acceptance
of the work package. The external program progress report is written only after
Mirza approves the activated candidate.

## Scope and ownership

The manager is a read-only projection of the existing instruction runtime,
composer, repository service, skill catalog and model roster. Its internal
protocol has no mutation operation. Runtime prompt/session source ownership,
Git publication, Startup Context, tool locking and context control remain with
their existing owners.

User workflow: [INSTRUCTION_MANAGER.md](../INSTRUCTION_MANAGER.md).

## Requirement-to-check mapping

| Requirement | Concrete evidence |
|---|---|
| `/instructions`, aliases and linked sections | Local physical Enter test across all six entry points; actual remote Enter/key route; command registry/help |
| Every kind, scope, invalid/effective/shadowed resource | Real temporary Git fixtures with every instruction kind, project external configuration, dedicated AGENTS.md, legacy files, external skills, roster aliases, invalid and missing resources |
| Redefinitions and high-impact warnings | Typed source-derived row flags, project-redefinition filter, synthetic global/project collisions, paired common/preferred semantics |
| Repository/branch/health/lease/gitlink | Existing real-Git repository service regression families plus manager projection; detached-state inspection and complete paged history |
| Exact unbounded source/detail | Multi-megabyte Unicode source reconstructed exactly through fixed transport pages; source edits after first page do not alter captured detail; stale document IDs rejected |
| Complete metadata and dependencies | Exact synthetic frontmatter, render versus validation-only graph relationships, reverse consumers, registered source/empty contracts |
| Render/component/system preview | Same composer operation and typed values as the domain, without bootstrap; isolated component supported, primary-unavailable full-system preview rejected |
| Notification and skill preview | Typed registration validation, visible missing occurrence values, exact compatibility skill parser, managed plain/Handlebars owner |
| Model roster preview | Current parsed global file and independent provider catalog; live local named-route resolution with no inference request |
| History, revision content and comparisons | Real Git per-resource and repository history, exact revision content, two-revision diff, working diff, scope comparison, and a 67-commit paginated tail |
| Search/filter/input/refresh/cancel | Shared reducer and actual local/remote key paths; source-derived filters; initial cancellation; repeated-search focus; pasted text kept out of the composer |
| Reconnect/stale responses | Session/request/snapshot/document/page matching, wrong-response-type rejection, actual daemon restart; ordinary History does not discard open detail |
| Responsive wide/narrow/extreme layout | Terminal-cell tests at 160x42, 80x24, 40x12 and 24x8; real PTY TUI at 150x40, 72x24 and 24x10 |
| Mouse and complete scrolling | Physical pane/view/filter/footer controls and wheel routing; long single-line page wrapping without a u16 total-document scroll limit |
| Content-safe diagnostics | Summary includes identities, booleans and counts, not source text. Explicit opt-in visual frames capture only visible terminal cells |
| No mutation or provider call | Store content, HEAD, ordinary index and status fingerprints; exact session prompt/message/model checks; localhost provider records zero inference requests |
| Protected prerequisites | Startup and Context Editor regressions, exact prompt preservation tests, unchanged disabled-memory policy and protected-path fingerprints |

## Mechanism suites observed

The verification run under `~/.jcode/scratch/wp09/final-verification/` recorded:

- Combined instruction/runtime/repository family: 69 passed at that boundary.
- Skill family: 38 passed.
- Model roster family: 8 passed.
- Internal protocol: 92 passed.
- Startup UI family: 47 passed.
- Context Editor families: 97 passed, 1 ignored.
- Root all-target compilation passed.
- Strict all-target Clippy over all changed Rust crates passed.

Final focused loops passed **12 inspection-domain tests and 10 manager tests**,
including invalid/unresolved-source diagnostics, initial cancellation, explicit
project-redefinition grouping, actual frame capture, reconnect-only refresh,
repeated-search target ownership, paste isolation, and control-character escaping.
Strict all-target lint and workspace formatting passed after these corrections.

### Baseline and correction accounting

Broad TUI suites retained the previously observed disabled-memory expectations:

- Remote family: 277 passed, one failure in
  `handle_server_event_applies_remote_memory_activity_snapshot`.
- Command family: 205 passed, two failures in the goal/side-panel tests that
  require globally enabled memory.

These are not passing aggregate suites. The Phase 2 policy was not weakened to
satisfy them. An initial side-panel baseline also reproduced the existing mouse
focus expectation failure. Changed manager input has its own passing real-key
and mouse checks.

The broad app-core client-lifecycle run exposed a pre-existing classification
bug: `SystemPromptDispatchError::StartupContext(Blocked)` was not recognized by
a classifier checking only the unwrapped `StartupContextDispatchError`. The
same code was present at the WP-09 starting revision. Blocking and exact prompt
rollback already worked, but the UI called unresolved requirements a persistence
failure. A separate narrow commit (`e4762de2c`) recognizes that existing wrapper.
The original assertion remains intact, and the complete family then passed
**30 tests**. No capture, receipt, context projection or rollback policy changed.

The first baseline launch omitted the scratch environment in the coordinated
shell. It ran no tests and was corrected. An early test filter matched zero and
was corrected, never counted as evidence. Intermediate compilation/formatting
failures during active edits were repaired and superseded by final checks.
The macOS test linker reported a compact-unwind size warning. Compilation and
execution succeeded; other operating systems were not exercised.

## Production acceptance probe

Run against a coordinated build or the activated binary:

```sh
python3 scripts/test_instruction_manager.py \
  --binary "$HOME/.jcode/builds/current/jcode" \
  --artifact-dir "$HOME/.jcode/scratch/instruction-manager"
```

It creates private disposable homes/projects, a local HTTP provider and real
Git stores. First-run onboarding is explicitly skipped through Escape. It uses
the production server protocol and actual PTY TUI keyboard/mouse paths, checks
fresh visible source frames, then restarts the daemon and rejects old snapshot
identities. All owned daemons/testers are stopped. It performs no paid inference,
real-account change or real instruction-store mutation. Artifacts remain for
review.

The first built candidate, `bc1e1b923`, passed the production journey with **226
catalog rows, 55 exact content pages and zero model calls**. Its artifacts are
`~/.jcode/scratch/wp09/live-vrj_kwce/`. Review of those frames identified that the
legacy tester IPC can return a frame before a requested redraw, so the versioned
probe explicitly waits for a fresh selected-source viewport rather than treating
an older extreme-narrow frame as final evidence.

Earlier live attempts identified an invalid absolute standalone fixture path,
an unimported conflicting project legacy/managed overlay, and first-run
onboarding consuming Enter. The fixture was corrected to the existing relative
path and source-precedence contracts, onboarding was skipped without importing
an account, and assertions were retained. The conflict also improved the
manager's diagnostic projection. None of those failed attempts is counted as a
successful valid-preview or physical-input journey.

## Final activated verification

The final production probe passed on **2026-09-06** against
`jcode v0.75.189-dev (3b9dc06bc, dirty)`, runtime
`3b9dc06bc-dirty-6ed7adf08941`. Current and shared channels matched and canary
passed. The active primary session retained its exact system-prompt state,
active-skill state, frozen routing text, model, authentication route and effort
across both reloads.

Final artifacts: `~/.jcode/scratch/wp09/activated-final/live-ijta3tml/`.
The versioned probe passed 226 catalog rows, exact reconstruction of 55 content
pages, high-impact grouping, repeated search between different resources,
actual remote keyboard and mouse paths, and content-matched frames at 150x40,
72x24 and 24x10. All six group/source viewports were inspected. The extreme-narrow
source frame shows the actual `WORKING` synthetic body at scroll 14, not an
older header-only frame. Source files, Git HEAD/index/status and session
instructions/messages/model remained unchanged. The provider recorded zero
inference requests. Daemon restart and stale-snapshot rejection passed, and all
owned test processes were stopped.

The final frame loop first found an assertion bug in the test helper's sentinel
parameter, then correctly exposed a pre-existing remote tester repaint defect:
`handle_tick` discarded the `check_debug_command` completion signal. The narrow
repair in `3b9dc06bc` requests repaint after an explicit debug-file command. Normal
physical keyboard routing was already passing. The probe also respects the
visual debugger's intentional deduplication of identical frames while requiring
the expected visible content. Earlier failed frame attempts are not counted as
successful narrow-body evidence.

Machine-readable summary: [WP09_EVIDENCE.json](instruction-manager/WP09_EVIDENCE.json).

## Acceptance boundary

This source/runtime result is a candidate, not accepted completion. User
approval, downstream publication, the external accepted progress report, and
WP-10 authorization remain separate lifecycle steps. The final evidence-only
commit does not change the activated runtime implementation.
