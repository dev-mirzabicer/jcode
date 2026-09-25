# Checkout closeout acceptance map

Status: production verification routes and recorded evidence. This is not Mirza's acceptance,
C01 completion, or a claim that the active shared runtime has been replaced.
The operating contract is [WORKSPACE_CLOSEOUT.md](WORKSPACE_CLOSEOUT.md).

The independent read-only review's F1–F3 were reproduced and repaired: source
metadata after interruption, independent registrations under quarantine, and
unavailable historical evidence stranding retention. Seven expanded regressions
passed and the same reviewer confirmed the original sequences are addressed.
The follow-up is bounded source review, not acceptance. The post-repair combined
suite passed 52 closeout cases, eight native descriptor cases and two owned
execution-supervisor cases, with the configured external volume selected. Exact
final binary, native-run, activation and publication identities belong in the
package completion ledger, not an inferred claim from these test counts.

## Requirement coverage

The approved WP-07 owns R25–R29. The rows below identify concrete production
owners and checks rather than infer coverage from aggregate test counts.
`closeout::tests` below is the `jcode-base::workspace::closeout::tests` family.
Tests use owned disposable files and synthetic content, not prompt-quality tests.

| Requirement | Observable contract | Concrete checks / production path |
|---|---|---|
| R25 authorization | Exact Location/generation, default-off conditional grant, trusted final approval, revocation, request replay and stale/foreign identity rejection | `authorization_is_exact_default_off_revocable_and_request_idempotent`; `final_authorization_is_review_bound_conditional_only_and_revocable`; `removal_rechecks_authority_after_concurrent_human_revocation`; protocol and shared Rust/TS reply-correlation matrix |
| R25 findings | Tracked/staged/conflicted, ignored/untracked/hidden, symlink/hardlink, stash/reflog/acquired refs, nested/submodule/LFS and linked-artifact facts stay distinct from dispositions | Inventory, bundle, `git_edges`, LFS and `combined_disposal` cases; `reference_inventory_retains_session_and_linked_document_without_hydration_or_rewrite`; `final_authorization_requires_preserved_linked_content` |
| R25 current versus historical | Resolved catalog-only blockers cannot be revived from an old inventory after quarantine | `reference_freshness::resolved_nested_reference_stays_resolved_through_quarantine_recovery`, including actual nested-checkout retain-files control and unchanged original reference bytes |
| R26 history | Independent bundle restore retains local refs, detached/stash/reflog and dangling/pseudoref roots; original/split/conflict/sparse indexes and submodule state remain recoverable without source | `bundle_restores_acquired_refs_detached_stash_and_reflog_without_source`; `git_restores_pseudoref_and_unreferenced_graph_roots_without_source`; ordinary/linked split-index tests; `git_edges::sparse_conflict_and_orphan_indexes_restore_without_source`; combined disposal validates separate restored repositories and LFS objects |
| R26 file data | Optional full archive or explicit dispositions, independent restores, metadata and link fidelity, external-reference independence; unknown/retained data blocks removal | Full-archive and external-reference tests, `explicit_directory_retention_overrides_archive_defaults`, opaque-path tests, metadata-only final-approval rejection; contents, ACL/xattr/resource-fork fingerprints and restored hardlink identity are checked |
| R26 failed capture/storage | Changed sources or archives, missing LFS payloads, unavailable/replaced destinations and failed backups never become preservation success | `final_authorization_rechecks_preservation_and_source`; interrupted scan; configured-filter isolation; descriptor archive replacement tests; historical LFS missing-payload refusal; `final_authorization_review_reports_committed_backup_failure`; combined unavailable-volume observation before and during removal |
| R27 work and admission | Closing blocks new relevant work, not just selected UI views. Existing ownership is observed without cancellation; aliases, ancestor readers and own-cwd constraints apply | Closing fence and source-lifetime tests; actual process cwd/open-file test; cross-namespace activity and process-death tests; specialized Startup Context/instruction/skill/delegation/attachment tests; native worker/hook ownership regressions |
| R27 pending and child owners | Queued input, incoming location control, unresolved execution and unreadable ownership are blockers; an actual hosted child stays running until its own release | `work_admission::pending_input_incoming_location_and_unresolved_execution_block_closeout`; app-core `closeout_live_child_inference_is_observed_without_cancelling_its_owner`. Both cases passed through their actual persistence/host owners in the recorded repair-integration run |
| R28 effects/recovery | Final authorization is rechecked under physical ownership, quarantine cannot replace data, entries have individual intent/receipt, interrupted effects remain unconfirmed, and Closed requires verified absence | Clean removal/history; six journal interruption boundaries; original-path replacement/new-entry tests; linked-worktree Git-effect reconciliation; owned execution Stop/resume; shared-supervisor adapter fixtures; real daemon/Harness/both-SDK journey |
| R28 evidence recovery | Catalog restore or lost receipts do not revive old authority or invent filesystem effects; human review can restart, resume validated state or retain files truthfully | `trusted_recovery_*`; `catalog_restore_cannot_reactivate_historical_conditional_authority`; reference-snapshot corruption and both older-journal cases. Unregistered/Retained is distinct from Closed |
| R28 compound filesystem | Submodule index/worktree state, two LFS versions, internal/external hardlinks and symlinks survive verified preservation, interrupted removal and loss of archive-volume observation | `combined_disposal_preserves_submodule_lfs_links_and_recovers_volume_loss`, optionally with an actual UUID-verified Active-volume destination. Outside targets/source refs remain untouched |
| R29 retained history | Closed/retained metadata, reports, manifests and portable references stay inspectable and cannot become fresh authority | Public history/progress and export/import fixtures; both SDKs read identical retained history through the real bridge; imported history does not fabricate a local operation |
| R29 conversation and cwd | Existing Session messages, frozen content, outputs and linked-page metadata survive removal; missing-cwd input is preserved and explicit repair does not wake inference or recreate files | `scripts/test_workspace_closeout_retention.py` and the separate `Session::capture_readonly`/execution-inspection probe; exact prior-message/content equality permits only the existing structural non-waking scope notice |
| R29 old grants | A retained grant cannot authorize unrelated new files at a closed Location's old spelling | Extended native retention fixture issues a real grant, repairs the Session, creates an explicitly owned replacement and requests a real native write. The real-daemon retention run passed with the old grant still Active in history and the native replacement-path write denied |

## Repeatable native routes

Use `scripts/run_isolated_test.py`, an explicitly selected immutable TUI binary
and an owned artifact directory. Never point fixture deletion at a real checkout
or interrupt a real daemon/drive to simulate failure.

- `scripts/test_workspace_closeout.py`: TypeScript SDK through the standalone
  Harness bridge and native daemon, including real disconnect, Stop, replay,
  preservation, review, removal, output and Rust SDK history comparison. Requires
  `JCODE_WP07_BRIDGE` and compiled ignored `JCODE_WP07_RUST_PROBE`.
- `scripts/test_workspace_closeout_retention.py`: intentionally creates one
  primary, uses a scripted localhost provider, and compares canonical durable
  Session/journal and execution results through a separately compiled probe.
  Requires `JCODE_WP07_SESSION_PROBE`. Managed primary launch is enabled only in
  that private fixture configuration.
- Base `workspace::closeout::tests::` uses the real Git/filesystem/catalog owners.
  Set `JCODE_WP07_EXTERNAL_MOUNT`, `JCODE_WP07_EXPECTED_VOLUME_UUID` and
  `JCODE_WP07_EXPECTED_OWNERSHIP` to opt into verified external-volume fixtures.
  Without that opt-in, skipped external cases are not external-volume evidence.

The final candidate ledger must name exact source and binary identities,
completed commands, artifacts and cleanup. A source build or older passing
canary is not activated-runtime evidence. Unit interruption hooks test journal
boundaries; they are not an unqualified claim about power loss or every I/O fault.

## Inherited regression boundary

Relevant existing tests preserve Startup Context capture/observation, managed
instruction precedence and blocked-source handling, external skill activation
and package-copy snapshots, original-parent delegation, source-free recovery,
owned execution capture/control, clone trust/materialization and physical identity.
No context-projection/curator algorithm, central prompt wording, dormant memory
or Swarm behavior is redesigned. Ordinary managed rollout and agent workspace
exposure stay gated. C04 final visuals and C05 final guidance remain separate.

## Evidence honesty and platform bounds

Native acceptance is macOS arm64. APFS rejects invalid-UTF-8 filenames; actual
native cases instead cover unusual Unicode/newline paths and opaque symlink
bytes, alongside codec checks for opaque filename representations. Arbitrary
shell/MCP/browser/computer effects and uncooperative same-user writers are not
physically sandboxed. File/cwd inspection is best-effort; unknown observations
block. Supported cooperating namespaces retain kernel-owned activity evidence.
No remote credentials, hosted Git provider, public upload or paid model is
needed for these checks.

External-volume ownership enforcement is observed, not changed or promised.
The delegated policy permits explicit weaker-protection destinations with
truthful review; it does not claim encryption. Volume disappearance is injected
through the read-only observation boundary, without unmounting a real drive.

Candidate acceptance, runtime activation, final publication and C01/parent
completion are separate. This file must not be used as an accepted completion
report until the independently recorded review and lifecycle are finished.
