# Phase 3 WP-03 migration evidence

**Implementation commits:** `ec5980b72`, `1c57f121d`, `2c158f186`, and `2b2096745`

**Starting source:** `6860452868b26ae534635a12753c789cd177b3ca`

**Starting tree:** `673d0bb405e2c3f5ec52522166b216c7a151f474`

## Boundary

This is one-time migration evidence for WP-03 system-composition rows. It is not a recurring prompt-prose test. The disposable equality probe described below was removed immediately after it ran.

## Compatibility agent equality

`INS-SYS-001` began as the embedded `crates/jcode-base/src/prompt/system_prompt.md` output. The WP-01 artifact records:

```text
bytes: 1558
sha256: 00a42f2fbefa67a8d96be95c7b79eb01c6298268d143e96a23f71287b9bfd72b
```

WP-03 creates `agents/jcode.md` from `DEFAULT_SYSTEM_PROMPT` and renders it through the production `InstructionRuntime::render_agent` path. A disposable integration test initialized a real temporary managed store, rendered `global:jcode` as a primary agent, compared complete UTF-8 bytes with `DEFAULT_SYSTEM_PROMPT`, and printed:

```text
WP03_COMPATIBILITY_AGENT bytes=1558 sha256=00a42f2fbefa67a8d96be95c7b79eb01c6298268d143e96a23f71287b9bfd72b
```

Command:

```text
cargo test --profile selfdev -p jcode-base --test wp03_migration_probe -- --nocapture
```

Result: 1 passed. `crates/jcode-base/tests/wp03_migration_probe.rs` was then deleted. The production suite contains no phrase assertion or snapshot of the compatibility prose.

## Other WP-03 inventory rows

- `INS-SYS-002`: the managed `system:mermaid` seed body is constructed directly from `prompt::MERMAID_PROMPT`. The typed runtime renders the complete plain body. Synthetic composition tests cover capability inclusion and exclusion.
- `INS-LEG-001` and `INS-LEG-003`: projects without a configured managed store retain the current project system-prompt and overlay compatibility paths. A configured managed project store uses receipt-gated cutover and fails on unreceipted duplicate legacy plus managed definitions.
- `INS-LEG-002` and `INS-LEG-004`: first global initialization imports present eligible sources exactly through the WP-02 import transaction. The composer deactivates each legacy source only when the manifest contains its durable receipt.
- `INS-EXT-001` and `INS-EXT-002`: `AGENTS.md` remains a dedicated live input. Each wrapper and file body remains unchanged. Their order intentionally changes from project-first to the accepted global-first order.
- `INS-LEG-005` and `INS-LEG-006`: preferred-tool files remain live compatibility inputs during WP-03. Their managed migration remains owned by WP-07.
- `INS-SYS-003`: the available-skills section keeps its existing renderer and bytes but is now captured in the activation snapshot. Managed prose migration remains owned by WP-05.

The complete full-system prefix changes intentionally because the approved agent-profile kernel is inserted and paired scopes move to global-before-project. Those changes are accepted product semantics, not migration-equality failures.
