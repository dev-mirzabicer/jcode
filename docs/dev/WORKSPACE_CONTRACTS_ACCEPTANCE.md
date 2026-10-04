# Workspace public contracts and agent activation acceptance ledger

**Candidate implementation evidence, 2026-10-04.** Covers SP-58-C01/WP-11
(R38): the [public workspace contracts](WORKSPACE_PUBLIC_CONTRACTS.md) and the
[agent `workspace` tool](../WORKSPACE_AGENT_TOOL.md). Mirza's acceptance,
activation of the user's runtime and downstream publication are separate steps
recorded in the external completion report.

Native acceptance is macOS arm64. The native journey uses an isolated state
root, an owned fixture daemon and bridge, disposable directories and Git
fixtures, and a scripted localhost provider. No paid inference, real checkout,
real catalog or the user's runtime was touched.

## Exercised boundary

Branch `mirza/sp58-c01-wp11-public-contracts` from `7bc478961`. The journey
passed against the repaired candidate (run 11), the activated `de10c6fbc`
build (SHA-256 prefix `d981aa4fc0e0bcdc`) and earlier `e544bf222`. Runs 1 to 6 failed
on journey-script defects (tool-name matching, placement shape, expected member
total, reading tool results from earlier turns), not product behavior.

Journey: `scripts/run_isolated_test.py python3
scripts/verify_workspace_contracts.py --binary <image> --artifact-dir <dir>`.
It drives real primary turns whose provider responses are scripted tool calls,
and uses the TypeScript SDK through the curated Harness bridge as the trusted
client.

## R38 evidence

| Requirement facet | Evidence | State |
|---|---|---|
| Harness/SDK cover the native contracts used by the reference client | `capability_coverage_tests::every_reference_client_capability_is_triaged` (failed at baseline `7bc478961` with seven untriaged requests; passes); Harness `workspace_probe`/`workspace` and `session_inspection_version`; bridge test `workspace_catalog_bridge_negotiates_correlates_and_keeps_closeout_separate`; SDK parity ledger | Validated |
| Rust and TypeScript typed tests with one correlation rule | `jcode-workspace-types` correlation tests and the shared `workspace_correlation.json` (19 cases, both outcomes) evaluated by Rust (`workspace_sdk_applies_the_shared_correlation_matrix`) and TypeScript (`workspace.test.ts`) | Validated |
| Version and unsupported cases | SDKs send nothing without `workspace_catalog_v1`, refuse requests whose native contract an older runtime lacks, refuse `closeout` on the catalog route, and gate `inspect_session` on `session_inspection_version` (Rust and TypeScript tests; bridge refusal test; journey refusal through the real bridge) | Validated |
| Large paged discovery | `memberships_page_completely_across_continuations` (routine); `large_memberships_page_completely_without_a_member_cap` (250 locations, ignored by default, passed in 1523 s); stale continuation is a conflict; journey pages 28 project members through the agent tool with `limit=10` | Verified |
| Tool exposure only with the human path | Staged configuration offers no session the tool (journey phase 1); `workspace_tool_is_exposed_only_to_placed_primary_sessions`; children's `ADMIN_TOOLS`; adoption adds the tool exactly once through a recorded tool-set change while earlier history is unchanged (journey) | Verified |
| No false approval from agent arguments | `workspace_tool_proposals_never_authorize_and_only_trusted_approval_grants` and the journey: forged `approved`/`trusted`/`session`/`grant` fields create only a pending proposal for the calling Session; writes stay denied until the SDK client approves; `workspace_tool_rejects_foreign_proposals_unplaced_sessions_and_agent_approval` | Verified |
| Narrowly authorized closeout | `agent_closeout_needs_scope_never_approves_and_finishes_only_its_conditional_declaration`, `agent_declaration_without_human_conditional_authority_is_refused_and_recorded`; journey: agent prepares, cannot approve, declares no loss under the human checkbox and finishes; the checkout directory is removed and the closeout reaches `closed` | Verified |
| Framework-stage operating text and deliberate prefix activation | Approved text stored verbatim as managed `tools/workspace.md` (seed 31); `workspace_guidance_freezes_with_its_tool_without_resetting_continuation`; journey: Session's frozen guidance equals the advertised description and survives a daemon restart byte for byte | Verified |
| Initial location facts | `location_context_reports_home_chain_scope_and_members_for_each_placement`, `initial_session_context_includes_location_facts_only_for_placed_sessions`; journey checks placement, work area and project identities in the stored initial context | Verified |
| C02 and C04 consumer fixtures | Journey: SDK `launchPrimary` plus `inspect_session` (C02), SDK proposal listing by page, review and apply, operation discovery (C04) | Verified |

Mechanism tests use synthetic content. None judges the approved operating text.

## Regression families

Run on the candidate source with private test homes where the suites provide
them: provider parity (5), workspace closeout runner and discovery (app-core
`workspace::`, 12), base `workspace::` serially (136, one ignored), TUI
workspace manager (31), tool-set planning (12, isolated home), context core
(60), instructions (137), Session (122), Startup Context (59),
Harness API, bridge, Rust SDK, workspace types and protocol (262), TypeScript
SDK (69). Strict Clippy (`-D warnings`, all targets) passes for every changed
crate.

Pre-existing failures, unchanged by this package: the two Swarm routing tests in
`tool::instruction_guidance` (reproduced with the baseline file), and in
`tool::tests` the disabled-Swarm `subagent_tool_is_not_registered`, the Phase 4
tool description budgets (`expand_tool_use`, `get_catalog`, `read_transcript`,
`session_outline`, `subagent`), and three batch tests that collide with
pre-existing `session_id='test'` rows in the developer's execution store
(created before this package). Two further `tool::tests` failures in the
parallel run pass serially.

## Adjacent repairs

Mirza's review asked about two problems the first journeys exposed. Both
predate WP-11: an earlier session from WP-10 shows the same `mcp`
removed/added churn, and the WP-07 use gate is the source of the slow reads.
WP-11 fixed both, because its agent tool and its tool-prefix evidence depend on
them:

- **Volume identity.** Every tool use or native write touching a registered
  root inspected every mounted volume with `diskutil`: about 1.5 s per root,
  3.6 s per tool call on a machine with 12 mounts. Identity checks now read the
  kernel UUID (`getattrlist`), which matches `diskutil` on every mount here
  (`native_kernel_identity_matches_full_inspection_for_every_mount`, plus the
  opt-in external-volume fixture on `/Volumes/Active`). Measured per-root
  verification fell to about 1 ms. Journey tool calls fell from about 4 s to
  0.04–0.12 s. Serial base `workspace::` fell from 7,122 s to 579 s.
- **Restored primaries keep their MCP tools.** `PrimaryHost::restore` built the
  Registry without MCP registration, so a reload continuation or detached input
  sent its first request without `mcp` tools. The tool set then announced them
  removed, and added again when a client attached. Restore now registers them like
  launch and new contexts do. Red then green:
  `restore_primary_preserves_busy_source_and_target_snapshots`. The journey now
  requires the full tool surface after restart and a single tool-set change at
  adoption.
- **Stack headroom.** The first version of that repair held the restored Agent
  inline across an await. In the `opt-level = 0` selfdev build, that added
  about 440 KB to each client task's stack and crashed the fixture daemon on
  attach (runs 9 and 10). The Agent is now boxed across the await. Measured
  frame totals for the crashing stack:
  - `de10c6fbc`: 1,790,224 B;
  - unboxed: 2,229,264 B;
  - fixed: 1,790,224 B.
  The pre-existing headroom of about 1.79 MB used out of a 2 MiB worker stack
  is handed to WP-12.

The journey passed again (run 11) on the repaired build.
