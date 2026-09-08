# Instruction manager mutation verification

**Status:** Candidate verification. WP-10 has not been accepted by Mirza.

**Final activated boundary:** `8f56ad57a`, runtime `8f56ad57a-dirty-efbe29180b11`, `jcode v0.75.211-dev`. Full combined acceptance ran at `712d37527`; the subsequent help/close-race changes have focused tests and an activated production TUI smoke. These boundaries are recorded separately, not conflated.

This record covers editing, Git operations, repository setup and recovery. The earlier read-only inspection record remains [INSTRUCTION_MANAGER_ACCEPTANCE.md](INSTRUCTION_MANAGER_ACCEPTANCE.md). Current user behavior is documented in [INSTRUCTION_MANAGER.md](../INSTRUCTION_MANAGER.md).

## Authority and architecture

- The instruction runtime retains discovery, scope, rendering and consumer contracts.
- The repository service owns prospective commit review, exact source/mode capture, scoped publication, physical-repository locks, setup and operation receipts.
- The management worker resolves opaque inspection targets using server-owned session context. Remote clients do not choose source paths by presenting filesystem strings as resource identities.
- The TUI owns unsent form/editor intent, named actions, confirmations and presentation. Server drafts and per-client recovery capsules are distinct from session prompt state.
- Dedicated `AGENTS.md` edits update the working file only. They never adopt or commit its enclosing project repository.
- Current system/skill/routing instructions remain session-owned snapshots. Management does not activate a profile or call a model.

## Requirement-to-evidence map

| Requirement | Concrete checks and observations |
|---|---|
| Complete client-local editor draft; source unchanged before Save | `editing::external::tests` covers quoted arguments, retained files, spawn/nonzero failures, divergence and terminal restoration errors. Separate-home production PTYs verify cooked editor input and unchanged server source before reviewed Save. |
| Typed metadata, resource identity, availability, template, addendum/default, roster candidates and effort | Typed forms/parser round trips, closed-choice tests, complete-value editor test, stale-form rebind test, and production manager create/redefine/addendum tests. Invalid roster aliases retain complete repair source. |
| Exact review and affected consumers | Prospective Git-tree tests reject uncommitted dependencies; production worker tests validate synthetic registered values, missing variables and empty required controls. Complete source versions and executable differences feed the diff. |
| Scoped Save and no-op | Real-Git tests preserve unrelated staged/dirty files; exact operation trailers distinguish IDs; completed retry preserves newer staging; unchanged metadata preserves original formatting. |
| Create global/project, redefine, addendum, clear and external commit | `manager_create_global_project_redefine_addendum_clear_and_external_commit_are_complete` traverses the production worker and real repository publisher for each named operation. |
| Rename/delete and reference repair | Production template tests distinguish semantic references from plain literal text. Same-repository repair is atomic. Active project cross-repository references/shadows block implicit migration. |
| Complete skill Copy/package operations | Production manager and existing skill tests preserve source identity, nested binary references, original source attribution and Unix executable modes through Copy, rename/delete and project redefinition. Historical package restore includes all reference bytes and removes newer files absent from the selected revision. |
| History compare, restore, deleted resource recovery and export | Existing inspection history tests plus `repository_history_restores_a_deleted_resource_and_exports_exact_bytes`, complete-package restore, and export traversal/binary preservation tests. Restore creates a new commit or a true no-op, never rewrites history. |
| Legacy import | Production project import test verifies exact body bytes, preserved original file and one managed-definition/receipt transaction using shared canonical import targets. |
| Ecosystem input editing | `ecosystem_save_is_private_reviewed_recoverable_and_never_commits_parent` verifies exact parent HEAD/index preservation. Duplicate ownership, stale external edits and completed-save retry preserve later working intent. |
| New/damaged global store, submodule, external and non-Git setup | Real-Git repository and operation suites cover initialization, explicit backup-preserving recreation, configuration races, relative submodule URLs, no parent commit, external attachment and missing-checkout repair. |
| Detached branch and explicit synchronization | Local bare-remotes verify fetch, fast-forward pull, exact-reviewed-commit push, remote branch retrieval and safe detached branch creation. Attached drafts block branch changes across aliases and state roots. |
| Conflict and repository-script safety | Hooks and clean filters are tested with real executable sentinels. Scoped saves reject pending merge/rebase/sequencer state, including staged conflict resolutions, without changing source, index or HEAD. |
| Stale drafts, disconnect/reload and response loss | Durable workspace tests cover two clients, exact generations, partial-write recovery, lost commit receipt and no-op persistence. Complete base/current/proposed comparison is required before creating a new reconciled draft. |
| Unsent local input | Partitioned recovery-capsule tests cover kernel ownership, archival, stale-write ordering, invalid records, symlink rejection and no automatic replay. Production client-process recovery verifies an unsent metadata value remains available without saving it. |
| Responsive keyboard/mouse, Back and confirmations | Manager cell/hit-region tests cover 150/80/60/minimum sizes, monochrome focus, UTF-8 input and stale confirmation identity. Production PTY journeys cover editor failure/retry, review and Save at 150×40, 80×24, 60×24 and 24×10. |
| Complete content over remote transport | Protocol tests reconstruct large Unicode replies from bounded chunks and reject wrong session, transfer or offset. The production large-draft journey records actual chunk events. The general wire-frame guard is retained; no managed-instruction size cap is introduced. |
| Protected session/context behavior | Complete journal-aware replay is compared before/after production mutation/restart. Final context-core, primary activation, disabled-memory and root compilation checks pass. No desktop/context-core/memory implementation path is in the package range. |
| Real global store | Activated read-only inspection reports Ready/main/schema 1/seed 27 at `c0af670a32ff2156693f27a2dab48d4dcb0043d2`. Tracked/untracked file hashes and index remain identical. The unrelated `.DS_Store` remains untracked. No real project or instruction remote was configured. |

## Observed checks

These are separate observed suites, not a fabricated aggregate count:

- Complete instruction family: **116 passed**.
- Final repository family after pending-merge protection: **53 passed**.
- Supplemental complete creation/resource-action journey: **1 passed**.
- Existing skill family: **38 passed**.
- Protocol library, including chunk transport: **95 passed**.
- Final manager TUI family, including contextual help and direct-close race: **42 passed**.
- Context-core library: **58 passed**.
- Primary activation/lifecycle family: **21 passed**.
- Global disabled-memory default regression: **1 passed**.
- Root `jcode --all-targets` compilation: passed.
- Strict all-target Clippy over base, app-core, instruction-types, protocol and TUI: passed. Final repository-only safeguard was rerun with strict base lint.
- Workspace formatting and scoped diff checks: passed at commit boundaries.
- Coordinated build/reload: passed. Current/shared identities matched and canary passed at `712d37527-dirty-5dc65a98bd61`; the final help/close-race tail receives a separate activation check below.

The full workspace test aggregate was not claimed. Existing unrelated disabled-memory and cwd-sensitive aggregate failures documented by earlier packages were not weakened or hidden.

## Reproduction

Use the coordinated self-development test wrapper for Rust checks. The primary filters are:

```text
-p jcode-base instruction:: --lib -- --test-threads=1
-p jcode-base skill:: --lib -- --test-threads=1
-p jcode-protocol --lib
-p jcode-tui instruction_manager --lib -- --test-threads=1
-p jcode-context-core --lib -- --test-threads=1
-p jcode-app-core agent::startup_context::tests:: --lib -- --test-threads=1
```

Production probes:

```sh
python3 scripts/test_instruction_manager.py \
  --binary "$HOME/.jcode/builds/current/jcode" \
  --artifact-dir "$HOME/.jcode/scratch/instruction-inspection"

python3 scripts/test_instruction_manager_mutations.py \
  --binary "$HOME/.jcode/builds/current/jcode" \
  --artifact-dir "$HOME/.jcode/scratch/instruction-mutations"
```

The mutation probe uses private server/client homes, a localhost recording provider and owned PTYs. It performs one synthetic localhost bootstrap turn to establish a conversation. Manager operations must add zero inference requests. It stores its exact source, protocol events, visible frames and compact evidence in its artifact directory and stops owned fixture processes.

Local implementation evidence is under `$HOME/.jcode/scratch/wp10/`. The completed combined run at `final-live/live-qcxstsk5` used `173339f38` and verified large drafts, all four sizes, editor failure/retry, server restart, unsent local-form recovery and exact canonical replay equality. The activated-binary run at `activated-live/live-p0idk3ap` passed direct PTY keyboard bytes, mouse Save, repository review/cancel/apply, all four sizes, server restart, unsent-form process recovery and exact canonical replay equality. Its compact record is [WP10_EVIDENCE.json](instruction-manager/WP10_EVIDENCE.json). It used one localhost bootstrap turn and zero manager inference requests.

## Failed attempts and evidence corrections

Failed attempts are not counted as passing acceptance:

- A manually allocated PTY lacked a controlling terminal. The fixture now assigns `TIOCSCTTY` through a fresh interpreter rather than a threaded `preexec_fn`.
- A probe sent Save while validation was still in progress, and later sent Escape before the Save receipt. The manager correctly ignored actions while busy. The probe now awaits the actual terminal state, not a matching source substring or an intermediate working-file write.
- A menu label was mistaken for the form that would open after selection. The probe now checks the modal transition and source-loading state explicitly.
- Snapshot JSON alone omitted journaled assistant messages. The final probe uses canonical `jcode replay --export`, which loads the session and journal, for exact replay comparison. It does not introduce a second journal reader or reduce assertions to counts.
- Before a real conversation, existing Jcode behavior refreshes generated initial session facts. One local bootstrap avoids treating that unrelated lifecycle rule as a manager mutation. No protected session-context behavior was changed.
- One live run ended in debug IPC timeout. Mirza reported accidentally stopping a server, but sandbox logs did not establish that as the cause. The failed run remains failed; direct PTY input removes debug-key timing from subsequent acceptance.

## Boundaries

Verification is on macOS arm64. Windows and every provider route were not executed. Git synchronization uses local bare remotes rather than hosted-account credential tests. Existing Git credential tooling retains custody of authentication. No paid inference, prompt-quality benchmark, central prose change, real project setup or instruction-store push was performed.

Fast-forward-only pull is explicit. Divergent history and already pending merge/rebase operations require deliberate Git resolution outside the manager, not hidden automatic merging or history rewrite. Source reads and already active session instructions remain distinct.

Human acceptance of the activated manager and permission to integrate/publish remain pending. A passing mechanism or live probe is not Mirza's approval and does not complete Phase 3.

## Final review tail

The final UI review corrected stale read-only help and made `?` open contextual draft actions. A direct Q-close racing an accepted update now retains the reply and then detaches the server draft, rather than retaining its lease behind a closed view. The new race/help tests and the full 42-test manager family pass. Strict base/TUI all-target lint also passes, including the supplemental resource-action journey. These are runtime-tail changes, not a claim that the earlier full live run used this later source.

Final activation and tail smoke passed at `8f56ad57a-dirty-efbe29180b11`: current/shared equality, canary, complete editing help, contextual Draft actions, and direct close detaching an in-flight review. Source/index/HEAD stayed unchanged and no inference request occurred. Artifacts: `$HOME/.jcode/scratch/wp10/final-tail/live-suj57kjr`. Initial smoke attempts encountered first-run overlays and an incorrect menu-title expectation; the actual UI was inspected, the probe assumptions corrected, and the complete tail passed. Only this evidence documentation differs from activated source after the final record commit.

## Revised UX candidate after field review, 2026-09-08

Mirza withheld UX acceptance of the earlier candidate. The passing mechanism
checks above did not establish that its mixed repository/resource navigation was
understandable. The September 8 decision round approved type-first navigation,
Effective-here/Global/Project views, source-aware edit actions, automatic review,
convenient cross-scope copies, visible external/source badges and separate
repository-administration and current-session pages. This remains WP-10, not a
new package or a task deferred to WP-11.

The new catalog presentation groups real source identities without reimplementing
runtime precedence. Skill grouping uses catalog-owned invocation identity;
unidentified invalid skills remain separate diagnostics. Scope copies preserve
original files, refuse destination collisions and warn about high-impact project
definitions. Selected content precedes technical metadata. Source view still
retains exact original frontmatter, and session snapshots show no misleading
source-scope selector or edit destination.

Observed checks: 14 inspection tests, 48 final manager tests, both cross-scope
copy tests and strict base/TUI/protocol/instruction-types lint passed. The full
instruction run passed 119 tests and exposed an obsolete assertion requiring raw
YAML in Overview. It was replaced by a structured availability assertion plus an
exact Source-view assertion, and the complete inspection family passed. Likewise,
the old duplicated scope-summary assertion now checks the dedicated visible scope
control. The 24×8 content-floor regression exposed during layout work was fixed
without weakening its content-access assertion.

`scripts/test_instruction_manager_ux.py` passed through real isolated server and
client homes at activated `014c0a6fe-dirty-1cb92b00cd4f`. It exercises type and
scope navigation, direct editor entry, automatic diff review, explicit Save,
global-to-project copy, collision explanations, separate setup/repository/session
pages, and skill names plus scope/external badges at 80/60/24 columns. It made no
model requests and preserved the running session's captured system prompt. Actual
frames were inspected, and a second pass removed redundant labels and the stale
scope indicator from session inspection. Exact evidence is recorded in
[WP10_UX_REFINEMENT_EVIDENCE.json](instruction-manager/WP10_UX_REFINEMENT_EVIDENCE.json).

The active category uses a [+] marker, while focus has its own cursor/border.
There is one border per pane, no extra nested frame, and no marker repeated on
every type. The narrow floor retains source badges, scope/setup controls and a
real content row. Existing pagination mechanisms are unchanged.

This revised result still requires Mirza's activated UX review and approval.
No accepted work-package closeout or downstream publication is implied.

Final revised UX activation is `5811f10da-dirty-fa5642aab7fd`, with current/shared
channels equal and canary passed. The final production run passed at
`$HOME/.jcode/scratch/wp10/refined-final-live/live-e193n5m6`, including real PTY
resize events through 80/60/24 columns, direct editor/automatic review, explicit
Save, global/project source selection, copy/collision handling, separate setup and
session pages, unchanged captured prompt and zero model requests. The final two
cross-scope tests, 48 manager tests and strict lint passed before activation.

Some intervening multi-process fixture launches/debug relays timed out. One
failed before Jcode produced output; another had a loaded manager in its terminal
log but no debug reply. No product success was inferred from those failures.
The final probe uses the same client-owned debug-file protocol directly, avoiding
the extra CLI relay, and resizes its established real client. Its earlier Global
filter was explicitly changed to Effective here before expecting a project skill.
That corrects the probe's assumption without weakening source-scope assertions.

The real global instruction store remains at
`c0af670a32ff2156693f27a2dab48d4dcb0043d2` with only its unrelated `.DS_Store`.
The five original dirty checkout paths remain byte-identical. Only probe and
verification-document updates differ from the final activated runtime source.
The revised candidate is awaiting Mirza's approval.
