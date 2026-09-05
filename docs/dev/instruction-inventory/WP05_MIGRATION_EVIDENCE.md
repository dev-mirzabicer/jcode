# Phase 3 WP-05 migration evidence

## Boundary

This document records one-time exact runtime-output evidence for the Phase 3 WP-05 inventory rows. It is a migration artifact, not a recurring prompt or skill prose test.

The disposable probe used synthetic skill content and the production pre-WP-05 and post-WP-05 rendering paths. It was run once on the WP-05 candidate source and removed immediately. No deterministic test remains that requires, forbids, snapshots, or judges consequential skill prose.

## Exact equality results

The probe compared complete UTF-8 output and reproduced every SHA-256 recorded in `PRE_MIGRATION_BASELINE.jsonl`:

| Inventory row | Compared production behavior | Bytes | SHA-256 | Result |
|---|---|---:|---|---|
| `INS-SYS-003` | Previous `prompt::build_available_skills_prompt` output versus managed `system:available-skills` rendering with the same sorted `SkillInfo` values | 230 | `8b9dd1916ddea4b67df5bed8f19c6c2323d58a1ae0063bcf3c102ea8c8e6b519` | Complete byte equality |
| `INS-SKL-001` | Existing external `SKILL.md` parse plus `Skill::get_prompt` versus an explicitly copied managed project skill invoked through the effective managed/external source model | 75 | `1b0c86c8ff5cb3ed17ffb5dbf558d6d84493a716c2087eb60f013aa50567b6f3` | Complete byte equality |
| `INS-SKL-002` | Previous active-skill dynamic wrapper around the selected skill output versus the stored active rendered-text composition slot | 91 | `f2307549363df6ef7c8bd004566a3ece18907874ddd570ec6dd1c7fa39346bea` | Complete byte equality |

The hashes exactly match the Phase 3 starting-revision artifacts. No wording exception was required.

## Mechanism evidence

Permanent synthetic mechanism tests cover:

- Managed project, external project, managed global, and external global precedence
- Effective and shadowed candidate inspection, existing external source ordering, stable read-only source identity, and explicit shadowed-source Copy
- Invalid or incomplete managed specificity without silent external fallback
- Isolation of unrelated invalid skills
- Plain literal braces and opt-in restricted Handlebars modules
- Complete global and project package Copy, nested and binary reference files, original source preservation, non-model-facing attribution, collisions, idempotency, matching partial/pre-commit retry completion, removal fallback, and symlink rejection
- Latest source at invocation, stable later turns after disk edits, reinvocation, registry reload semantics, resume, process reconstruction, split, system replacement, and active-skill persistence
- Local and remote slash invocation, trailing prompts, images, multi-word names, remote-only server catalog names, bare activation, authoritative confirmation, and turn-coupled server ordering
- Managed available-skills typed rendering and frozen system activation
- Raw, Markdown, replay, startup-stub, clear, and transfer behavior through the existing Phase 3 session suites

## Installed source preservation

WP-05 does not rewrite imported or existing skill prose. The project skill present at implementation startup remained outside the managed store and byte-identical:

```text
fe0427bb421c2ea1089aa7af2b0f0d59242e876926a83b9a73359641e4f7fddd  3382  .jcode/skills/optimization/SKILL.md
```

The two historical OpenCode skill files observed during grounding were not current Jcode skill sources and were not modified or imported.

## Expert-review corrections, 2026-09-05

The expert reviewed WP-05 through `82b9af2c7` and exercised the combined
WP-05/Astra tree at `501f0738a`. Four findings reproduced before correction:

| Finding | Correction owner | Regression evidence |
|---|---|---|
| Remote `/skills` sent `ActivateSkill("skills")` | Remote slash input calls the shared built-in dispatcher before unknown-name skill fallback | `remote_enter_preserves_shared_builtin_command_dispatch` exercises real remote Enter for `/skills`, `/config`, `/help`, `/version`, and `/config show` |
| External global invocation returned cached OLD after a disk edit | `SkillRegistry::activate` rereads the selected external file, validates the invocation name, and returns a new independent activation | `external_invocation_rereads_selected_source_without_registry_reload`, app-core and local TUI global-source lifecycle tests, and `skill_manage load` shared-registry test |
| Copy paired a captured NEW original with an OLD managed body | Copy parses metadata and body from the exact captured `SKILL.md` bytes, using the same compatibility parser as invocation | `copy_parses_captured_skill_bytes_and_rejects_changed_identity` verifies body, description, permissions, references, retained original, and name-change rejection |
| Invalid UTF-8 managed source exposed external fallback when ID differed from name | Catalog captures recoverable invocation hints from the same bytes as validation; unknown invocation identity is an explicit catalog error, not a stable-ID fallback | Both-scope `unreadable_managed_name_cannot_expose_external_fallback` and captured-hint regression cover invalid UTF-8, malformed/empty metadata, nonregular targets, and unidentifiable paths |

The expert's three original standalone source probes also passed unchanged
against the corrected production APIs. Their disposable test file was removed
after execution. Permanent regression fixtures contain synthetic instructions.

The complete remote client family yielded 35 passes and one failure in
`handle_server_event_applies_remote_memory_activity_snapshot`, which expects
memory activity while downstream memory is disabled. That test and the
production remote event handler are unchanged from `501f0738a`; the existing
disabled-memory test boundary is not counted as a pass. The specific built-in
command regression and all local/remote skill tests passed.

These corrections do not change Astra, the accepted skill lifecycle, prompt
prose, provider projection, or disabled-memory policy. They correct the
mechanisms required by the existing contract. This record is verification
evidence, not a statement of Mirza's work-package acceptance.
