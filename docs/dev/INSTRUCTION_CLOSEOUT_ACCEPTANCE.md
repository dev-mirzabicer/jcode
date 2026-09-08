# Independent Phase 3 closeout verification

**Status:** Candidate verified on 2026-09-08. Final Phase 3 acceptance is pending.

This is independent combined-source evidence, not a replacement for the original
[migration inventory](INSTRUCTION_INVENTORY.md) or [requirement/journey map](INSTRUCTION_INTEGRATION_ACCEPTANCE.md).
No acceptance is inferred from work-package reports alone.

## Source and runtime boundary

- Phase baseline: `6860452868b26ae534635a12753c789cd177b3ca`.
- Independent closeout started at `5fdd211447cb33beef520553dbb0b4dffcb33e0c`.
- The original combined range contains 146 commits and 625 changed files. Its
  complete chronology, footprint, dependency changes and protected-seam deltas
  were inspected, followed by caller-to-owner source review of the critical paths.
  This is not a claim of a fresh line-by-line review of all 74,433 added lines.
- Repair: `cec136c7d`, `fix(instruction): reject Git metadata path aliases before access`.
- Documentation: `fb05a0e9122500a8bdf35d347cbe2632c6af4cb1`, tree
  `baaeae498ff1c3b91f7522f1ee4d7fc27f50aa27`.
- Activated implementation: `jcode v0.75.239-dev (fb05a0e91, dirty)`.
- Runtime: `fb05a0e91-dirty-129501add786`. Current and shared channels match,
  canary passed, reload reached SocketReady on 2026-09-08 at 22:08 UTC.
- Later evidence-only commits do not change that tested implementation.
- Closeout branch: `mirza/phase-03-closeout`. Integration/publication requires
  the candidate's explicit user approval.

## Findings and repairs

### Git metadata aliases

The shared managed-path validator rejected only the exact component `.git`.
On the current case-insensitive macOS filesystem, `.GiT` addresses the same Git
metadata directory. Independent public repository API regressions proved:

1. `read_file` returned `.GiT/config` as instruction content.
2. A rejected `commit` had already written a synthetic file inside Git metadata.
3. A malformed seed reached Git processing instead of rejecting the path before
   materialization.

The validator now rejects `.git` case-insensitively at every component. This is
one correction at the existing owner, shared by reads, drafts, mutation,
initialization and committed-snapshot materialization. Ordinary names such as
`.github`, `.gitignore` and `.gitmodules` remain valid.

All three regressions failed on the old implementation and pass after the fix.
The existing exact-blob snapshot test now also rejects a mixed-case alias.
The commit regression verifies unchanged HEAD, index and config plus absence
of the rejected write. No real instruction repository was used for these probes.

### Documentation and authority reconciliation

Current-source compatibility rendering can initialize a genuinely new global
store through the same preparation used by primary composition, notifications
and roster reads. The old assertion that internal non-primary callers never
initialize it was stale. The store, migration and system-prompt guides now
separate side-effect-free construction/inspection from source-consuming rendering.
No runtime bootstrap policy changed during closeout.

The private WP-03 specification also retains a supersession note for its old
explicit-global fallback sketch. The accepted implementation and later lifecycle
clarification resolve effective unqualified `jcode`; explicit `global:jcode`
remains available. No new default was selected here.

## Independent requirement reconciliation

Every row below refers to the final repaired source and actual observed checks.
Detailed test names and original owner contracts remain in the linked combined
map. Counts describe observed suites, not a substitute for this mapping.

| Requirement | Independent evidence |
|---|---|
| R-01 | All 129 inventory IDs and final dispositions read and reconciled, including 28 original exclusions and later NTF-001/SYS-006 dispositions. Compatibility source sinks checked. |
| R-02 | Runtime/composer production ownership traced; final 130-test instruction family includes complete caller operations and source-isolation checks. |
| R-03 | Final resource-kind, metadata, scope and semantic round-trip tests, with skill packages and roster kept in their proper domains. |
| R-04 | Deep finite graph/large synthetic content, restricted-template/cycle failures, complete remote paging and large draft transport checks. |
| R-05 | Project-first/explicit scope, invalid shadows, ambiguity, unrelated invalid-resource isolation and damaged compatibility-profile recovery tests. |
| R-06 | Empty/missing/deleted-resource and seed-history tests, plus live clear/delete/restore and collision behavior. |
| R-07 | Typed registration/consumer checks, source-traced structural ownership, retained curator/memory/provider/schema/selfdev exclusions. |
| R-08 | Real initialization, permissions, damage/recreation and recovery tests; new reserved-path seed regression; unchanged real-store identity and hashes. |
| R-09 | Real submodule, external and standalone repository tests, setup/branch/repair contracts and live repository controls. |
| R-10 | Complete repository tests and activated mutations, scoped index/parent preservation, binary modes, locks, stale drafts, history, restore and local-bare-remote sync. New metadata-write regression. |
| R-11 | Receipt-gated import/legacy compatibility and AGENTS.md order tests, live legacy/ecosystem editing and complete skill Copy. |
| R-12 | Final manager tests and activated 226-row/55-page inspection, physical keyboard/mouse at four sizes, history/Back/reconnect and source immutability. |
| R-13 | Activated separate-client editor failure/retry, automatic review, explicit mouse Save, setup controls, exact diff, stale recovery and genuine client/server restart recovery. |
| R-14 | Original/current compatibility and Mermaid captures reconciled; unchanged central prose has Mirza's recorded 2026-09-08 17:20 UTC approval. |
| R-15 | Final primary provider-recording tests and actual daemon lifecycle probe retain exact system/skill text after source Save and later requests. |
| R-16 | Final primary/client lifecycle suites plus actual CLI, REPL and Harness explicit/default/invalid/repeated-create/peer-preservation checks. |
| R-17 | Source-reviewed session persistence, dispatch and activation; resume/split/clear/transfer tests and actual split/daemon restart/clear journey. |
| R-18 | Actual append-only profile, same-agent no-op and unchanged route/skill/system checks, plus final idle-boundary and atomicity regressions. |
| R-19 | Final replacement preflight/rollback/cache/continuation tests and live complete latest-source replacement without immediate inference. |
| R-20 | Session structural validation and context owners reviewed; final context commit/draft tests and live locked row, pre-curator rejection, rewind/undo and split. |
| R-21 | Final export/replay/protocol/SDK tests, canonical journal-aware replay and exact on-demand instruction inspection. |
| R-22 | Final 38 base/20 TUI skill tests, complete package/mode/binary tests, live edit/freeze/reinvoke/split/restart/clear. Direct SDK activation remains explicitly unavailable. |
| R-23 | Final notification/todo/recipient/Startup Context owners exercised in base/app suites; all 407 WP-06 one-time comparisons exact. |
| R-24 | Final workflow/SDK/command/transfer/review/ambient/overnight/task-control suites; 354 WP-07 cases with exactly three approved data corrections. |
| R-25 | 99 unique original baseline records, all 16 captured-record hashes recomputed correctly; final 24 compatibility outputs exact; migration exceptions and six preserved-whitespace assets reconciled. |
| R-26 | Accepted five-alias global roster and unchanged live seed, final parser/validation tests and structured manager editing. No alias or central description changed. |
| R-27 | Final resolver/override/error/concrete-restore tests plus two production native/named/pinned route construction tests. No inference/quota guarantee claimed. |
| R-28 | Final fresh-creation/attachment ordering/no-orphan checks, source errors, live stale/crash/receipt recovery and absent-only HEAD fallback tests. Future isolated/async callers remain typed primitives, not implemented jobs. |
| R-29 | Final context-core, Startup Context, hard-disabled memory, tool-lock/late-MCP, KV cache, Anthropic split/prefix, OpenAI continuation/verbatim system and context-window checks. |
| R-30 | Current shipped guides reviewed against source, stale bootstrap claims corrected and relative links checked. |
| R-31 | Scoped commits, protected hashes, no desktop changes, strict affected all-target lint, root all-target compilation, formatting/diff checks, coordinated build/reload and activated probes. |
| R-32 | Future-owner interfaces and safe pre-adoption behavior reconciled with live source. No implicit delegation, async, role corpus, skill corpus or model-policy adoption. |

## Final source test observations

The full versioned `scripts/test_instruction_integration.sh` ran on `fb05a0e91`.
Its result is **nonzero**, with the documented baseline failures below retained.

| Suite | Observed outcome |
|---|---|
| Complete base | 1,492 passed, 16 failed, 1 ignored. Includes all 130 instruction tests passing. |
| Fresh base families | Skills 38, roster 8, prompt 19, transfer 4 and Startup Context 55 passed. |
| Complete app-core | 1,405 passed, 3 failed, 25 ignored. Primary activation/lifecycle and context-projection checks passed. |
| Shared domains | Context core 58, command risk 67, plan 78, protocol 95, provider core 142, Claude CLI 3, session types 14, overnight 8, Swarm core 13 and task types 7 passed. Instruction-types has no standalone tests; its transport semantics are exercised by protocol tests. |
| OpenRouter | 121 passed, 1 ignored. |
| Harness | API 16 and bridge 70 passed. |
| Rust SDK | 10 library, 15 client, 5 lifecycle, 5 structured-output and 1 doc test passed. |
| TUI focused | Manager 48, profile 2, skills 20, Startup Context 47, Context Editor 97 with 1 ignored, replay 6, KV cache 11 and message crate 9 passed. |
| TUI broad | Commands 206 passed/2 failed; remote 277 passed/1 failed. |
| TypeScript | Typecheck/build and 47 tests passed, without installation/publication. |
| Supplemental | Anthropic 24, OAuth/API split 2, OpenAI continuation 1 and verbatim system 1, production roster 2, context-window 6 and memory-default 1 passed. |
| Quality gates | Strict all-target Clippy over affected domains, root all-target compilation, workspace format check and working/closeout-range diff checks passed. |

Base failures are the same twelve disabled-memory/goal expectations, three
cwd-sensitive session-context expectations and one related skill fixture.
App-core's three failures, the two TUI command failures and the one remote failure
also expect globally enabled memory. Their names and errors are preserved in the
logs. No memory policy, assertion, ignored flag or test budget was weakened.

The first supplemental root roster filter matched zero and was rejected by the
developer wrapper. The corrected source-confirmed `roster_` filter ran both tests
successfully. The original supplemental runner remains recorded as failed.
An earlier pre-repair matrix was intentionally superseded after its completed
base baseline and focused checks; it is not represented as a completed matrix.

The complete historical phase diff still reports six migrated workflow files'
intentional trailing/newline bytes. Their equality/exception mapping is in
[the reconciliation](instruction-inventory/WP11_RECONCILIATION.md). Closeout did
not trim accepted prose to make that historical check green.

## Activated journeys and visual review

All five versioned production probes ultimately passed on the exact activated
`fb05a0e91` binary, without changing their assertions or response deadlines:

- `test_instruction_callers.py`: actual CLI/REPL/Harness creation, invalid-source
  cleanup, repeated fresh sessions, defaults, reattach and preserved peers.
- `test_instruction_lifecycle.py`: source Save versus exact active instructions,
  append/no-op/replacement, context lock, rewind/undo, split, reinvocation,
  daemon reconstruction and clear.
- `test_instruction_manager.py`: 226 rows, complete 55-page reconstruction,
  source/index/session immutability, history/comparison, physical keys/mouse and
  restart/reconnect at 150x40, 80x24, 60x24 and 24x10.
- `test_instruction_manager_mutations.py`: complete source/package/import/ecosystem
  operations, separate-client editor failure/retry, mouse Save, reviewed Git
  operations, real PTY resize, client-restart unsent form and server-restart draft
  recovery. Canonical journal-aware replay remained unchanged.
- `test_instruction_manager_ux.py`: type-first scope/source browsing, direct editor,
  automatic review, explicit Save, project Copy/collision, external skill labels,
  setup/repository/current-session pages and actual resize.

Independent frame review covered all four sizes, contextual Actions, content-first
source ownership, narrow skill labels, exact diff/recovery forms, copy conflicts
and the distinct active-session snapshot. Reviewed frames reported no anomalies.
At 80/60 columns one focused pane wins. At 24 columns compact Actions/Back remain
reachable and content scrolls. Normal panes use one border depth; menus expose
long labels/details without treating source truncation as complete content.
No unrelated visual redesign was performed.

The lifecycle probe made three localhost recording-provider requests. The
mutation probe made one localhost bootstrap and zero manager inference requests.
Caller, read-only and dedicated UX probes made zero inference requests. No paid
model or hosted-provider behavioral test was performed.

## Failures, recovery and retained limits

- One physical attempt failed before daemon startup because the chosen scratch
  path exceeded macOS `SUN_LEN`. Shorter fixture paths corrected the probe only.
- Another attempt passed mutation APIs but hit ENOSPC during physical editor
  testing. Only 19 inactive incremental-cache entries, approximately 2.7 GiB,
  were manifested and removed. No source, binary, session, instruction history or
  prior evidence was deleted. The activated binary's signature verified.
- A subsequent complete mutation/restart journey passed. The following extra UX
  probe was deliberately stopped when monitored disk headroom dropped below
  512 MiB. Its stop is not a pass. After headroom recovered, the dedicated UX
  probe passed in isolation with a 1-GiB early-stop guard.
- Native samples record transient disk pressure and `syspolicyd` activity/PID
  changes. Earlier work already reported this system-service boundary. Closeout
  does not claim to have repaired macOS or proven a new Jcode cause, and did not
  weaken system security. Failed attempts remain distinct evidence.
- Runtime acceptance is macOS arm64 with localhost providers and local bare Git
  remotes. Windows/other platforms, every hosted provider and quota availability
  are not claimed tested. Prior platform/opaque-provider bounds remain.
- Direct SDK skill activation remains a documented API gap. Existing TUI/REPL
  activation and SDK inspection are supported. Future owners must adopt new
  execution policies explicitly.
- Central kernel/transition/replacement/roster prose was unchanged. Mirza's latest
  recorded text review was 2026-09-08 at 17:20 UTC; prior accepted UX field review
  complements, but is not substituted for, the independent production probes.

## Preservation and reproduction artifacts

The real global store remains `main` at
`c0af670a32ff2156693f27a2dab48d4dcb0043d2`, schema 1, seed 27. All 213 tracked
files, its index and all thirteen dormant memory files match the initial hashes.
The five pre-existing checkout dirty/untracked paths remain byte-identical.
No real project store, instruction remote, upstream branch or central prose was
changed. No swarm delegation was used. An unnecessary `selfdev enter` created a
cloned terminal client; its exact owned process was stopped without delegating
work. No duplicate worker was used for the audit.

Main artifact root: `~/.jcode/scratch/phase-03-closeout-20260908/`.

- `final-matrix/run-PAKGwZTt/`: full final-source logs/results and source identity.
- `git-alias-red.log`, `git-alias-green.log`: reproduced defect and fixed checks.
- `supplemental/`: provider checks, original zero-match and corrected roster run.
- `typescript.log`, `artifact-review.json`, `instruction-tests-final.json`.
- `live/callers/`, `live/lifecycle/`, `live/manager/live-b5ed_uxm/`: passing probes.
- `~/.jcode/scratch/phase03-live-retry/manager_mutations/live-jrhv63oj/`: passing
  complete physical mutation and both restart-recovery boundaries.
- `~/.jcode/scratch/phase03-ux-final/manager_ux/live-tu5zrh4n/`: passing isolated UX.
- Earlier `live/manager_mutations/`, `~/.jcode/scratch/phase03-live/` and retry
  resource logs retain failed/guard-stopped attempts.
- `store-before.json`, `memory-before.json`, `preservation-final.json`,
  `incremental-cleanup.json`, complete phase chronology/path/diff artifacts and
  `WORKING_AUDIT.md` retain non-content safety and audit evidence.

No known in-scope product defect remains after the closeout repair. Final human
phase approval, downstream integration/publication and the authoritative private
completion/capability-map/roadmap updates are still required.
