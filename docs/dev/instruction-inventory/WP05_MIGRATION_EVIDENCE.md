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
