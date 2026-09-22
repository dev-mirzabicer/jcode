# Workspace scope verification

This guide maps the scoped-permission implementation to its acceptance paths.
It is not implementation acceptance and does not enable ordinary managed rollout.
Use the current [native scope](NATIVE_WRITE_SCOPE.md),
[catalog](WORKSPACE_CATALOG.md), and
[new-context scope](../PRIMARY_CONTEXT_SCOPE.md) contracts.

## Requirement map

| Requirement | Production owner and checks |
|---|---|
| R09 legacy and new contexts | Explicit adoption through workspace location controls, Session checkpoint/reconciliation and PrimaryHost. `legacy_adoption_checkpoints_one_notice_without_rewriting_history_or_frozen_state`, `legacy_adoption_uses_the_real_idle_primary_control_without_inference`, `context_scope_carry_is_reviewed_checkpointed_independent_and_replay_safe`, `shared_clear_carries_only_reviewed_direct_grants_as_independent_authority`, `split_and_transfer_require_reviewed_carry_and_keep_context_semantics`, Rust/TS capability/target tests. Native permission script exercises real split and SDK Clear/drop. |
| R15 placement and roots | `workspace::scope` over authoritative Session placement and current members. `scope_follows_placement_current_members_and_explicit_audiences_not_cwd`, `grants_do_not_chain_through_target_organization`, `context_cwd_uses_live_inherited_or_reviewed_direct_grants_not_a_scope_override`, native nested-root/submodule fixtures. |
| R16 trusted grants | Revision-bound review/application, exact pending proposal, source-qualified import binding and single-use carry. `direct_proposal_approval_is_exact_and_survives_reconnect_and_paging`, `permission_decisions_and_import_bindings_retain_history_without_implicit_authority`, protocol forged-authority negatives, native admin approval and copy independence. |
| R17 durable revocation and notices | Current-policy admission after delay, short in-flight permits, independent append-only Session observation receipts. `grant_review_conflicts_replay_and_checkpoint_faults_are_atomic`, `scope_explanation_receipts_follow_policy_not_organization_noise_or_history`, `scope_notices_are_durable_non_waking_and_do_not_delay_revocation`, actual external pre-tool-hook revocation and retained child Registry tests. |
| R18 every native destination | Actual parsed `NativeFilePlan`, verified descriptor-relative effects, all-path preadmission. Full `location::native_files::tests`, `tool::tests::native_scope` and existing `tool::apply_patch` tests. Native script covers read/write/edit/multiedit/unified/Codex patch and late-target rejection. |
| R19 child intersection | Frozen originating permission/MCP policy plus current original-parent workspace authority. Original isolated suite and native child fixture test read-only artifacts, permission upgrades/downgrades, parent move/revoke, missing parent and administration denial. |
| R20 callers and gates | Registry aliases, nested batch and concrete mutators acquire the same code-owned policy. Session-owned route guard covers both Agent loops and direct local TUI request preparation. Managed opaque providers and unadopted legacy dispatch fail before inference. Captured native state namespace and managed-history contradictions fail closed. |

## Explicit destination inventory

| Native surface | Admission/effect contract |
|---|---|
| write | Leaf and required parent creation use one verified permit. |
| edit/multiedit | Same captured target for read and write, with existing partial-edit semantics retained. |
| unified patch | Every parsed target, including removal entries, admitted before first effect. |
| Codex apply_patch | Every parsed add/update/delete and both move endpoints, with entry-aware unlink and metadata-preserving move. |
| batch and aliases | Each nested call goes through the originating Registry and invocation policy. No new whole-batch atomicity promise. |
| side panel | Specialized private page owner. Loading a linked source does not grant write-back to that source. |
| read/search/agentgrep | Internal derived output/index state uses its existing owner. Ordinary source reads have no workspace write-approval gate. |
| browser/computer captures | Internally allocated observation artifacts use the existing capture owner. External scripts/eval/provider commands are not sandboxed. |
| bash/CLI/MCP | Arbitrary effects remain instruction-led, not falsely claimed to be path-enforced. |
| instructions/runtime/catalog/session | Specialized trusted services retain their authorization. Generic native writes cannot manufacture permission by modifying their control stores. |

File/directory-symlink removal, ordered delete-then-create, case/Unicode aliases,
missing descendants, traversal, retarget/replacement, multiply linked files,
submodules and changed Git indirection have distinct checks. Native admission is
not proof against arbitrary same-user external filesystem writers. Later I/O
failure retains partial-effect truth, not fabricated rollback.

## Native reproduction

Use an explicit immutable binary and separate owned state:

```sh
python3 scripts/run_isolated_test.py python3 scripts/verify_native_write_scope.py \
  --binary /absolute/path/to/candidate/jcode --artifact-dir /short/private/scratch
python3 scripts/run_isolated_test.py python3 scripts/verify_primary_tui.py \
  --binary /absolute/path/to/candidate/jcode --artifact-dir /another/private/scratch
```

The first uses actual daemon control operations, native tool effects and the
TypeScript SDK against a localhost scripted provider. The second uses physical
PTY input/debug frames, busy navigation, independent Clear and narrow reconnect.
Each run retains result, events/provider evidence and owned-process cleanup.
A passing fixture process alone is insufficient if its retained effect, identity,
terminal or source data contradicts the claim.

Final signoff must record the exact source/binary, full affected regressions,
strict lint/format/diff results, runtime activation/canary and protected hashes.
Failed fixture assumptions, disk-guard interruptions and unrun checks stay separate
from passing evidence. Native macOS testing is not Linux/Windows parity or hosted
provider/model-quality evidence. C04 final management visuals and C05 final corpus
remain outside this work package.
