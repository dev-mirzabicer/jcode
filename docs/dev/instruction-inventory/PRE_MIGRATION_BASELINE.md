# Pre-migration instruction baseline

**Starting source:** `6860452868b26ae534635a12753c789cd177b3ca`

**Starting tree:** `673d0bb405e2c3f5ec52522166b216c7a151f474`

**Ledger:** [`PRE_MIGRATION_BASELINE.jsonl`](PRE_MIGRATION_BASELINE.jsonl)

## Purpose

This directory is the one-time WP-01 preservation artifact for every inventory row that a later Phase 3 package may migrate. It exists before any production caller migration.

It is migration evidence, not a permanent prompt snapshot test. Later packages compare their migrated production result against the applicable artifact once, record equality or a Mirza-approved exception in the inventory, and do not turn the prose into a recurring deterministic test.

## Evidence forms

Every migration row has exactly one JSONL record with:

- Stable inventory ID
- Exact starting source revision and tree
- Current source and consumer
- Complete delivery and representative-fixture contract
- Production reproduction route
- Target resource and owning package
- Evidence kind

Two evidence kinds are used.

### `captured_representative_output`

The artifact stores the complete rendered output, representative typed/file inputs, and SHA-256 of each output. `pre-migration/core-prompt-outputs.json` was produced by a disposable test that called the production prompt, legacy-source, skill, and Swarm-guidance renderers. The probe was removed immediately after capture, so no prompt-prose assertion remains in the test suite.

### `immutable_renderer_recipe`

The artifact freezes the exact starting Git tree, source/consumer identity, complete delivery and fixture contract, and named production rendering route. This is the authoritative pre-migration artifact for occurrence-time instructions and data-dependent builders where there is no single canonical output independent of runtime values.

This form is deliberate rather than deferred:

- The starting Git tree is immutable and contains the complete renderer bytes.
- The record names every dynamic value and structural input needed to reproduce a representative occurrence.
- The record names the exact production route, not merely the Rust source spelling.
- Later comparison must invoke that starting renderer contract and the migrated renderer with the same representative values.
- User, project, task, path, provider, time, error, and external-source data are not copied into a supposedly canonical prompt snapshot.

The Phase 3 product and acceptance contract allow exact runtime bytes **or a reproducible renderer**. This ledger instantiates that choice now for every row. No row says that baseline capture should be invented later.

## Captured core outputs

`pre-migration/core-prompt-outputs.json` contains complete outputs for 16 core rows:

- Built-in compatibility-agent body
- Project/global base replacements
- Mermaid module
- Project/global `AGENTS.md` wrapping
- Project/global overlays
- Project/global preferred-tool wrapping
- Available-skills section
- Skill body and active-skill wrapper
- Swarm routing guidance
- Light and deep Swarm effort directives

The fixture content is synthetic. The Jcode-authored wrappers and prose are the real production output from the starting implementation.

## Completeness check

The artifact set is complete only when:

1. Every `pending` migration row in `INSTRUCTION_INVENTORY.md` has one JSONL record.
2. No exclusion row appears as a migration record.
3. Every record names the exact starting revision and tree.
4. Captured outputs' SHA-256 values match their UTF-8 text.
5. Every evidence kind is either `captured_representative_output` or `immutable_renderer_recipe`.

WP-01 verification checks those properties structurally. It does not inspect required phrases or judge prose quality.
