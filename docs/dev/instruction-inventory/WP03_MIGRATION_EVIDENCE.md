# Phase 3 WP-03 migration evidence

**Core implementation commits:** `ec5980b72`, `1c57f121d`, `2c158f186`, and `2b2096745`

**Expert-review correction commits:** `0a6edb085`, `895a00640`, `4439ff7db`, and `f45a1e3c6`

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
- `INS-LEG-001` and `INS-LEG-003`: project compatibility sources contribute only when the corresponding managed project resource is genuinely absent and no import receipt completed cutover. Invalid or ambiguous managed candidates remain authoritative failures. Explicit global selection cannot be replaced by project legacy input.
- `INS-LEG-002` and `INS-LEG-004`: first global initialization imports present eligible sources exactly through the WP-02 import transaction. The composer deactivates each legacy source only when the manifest contains its durable receipt.
- `INS-EXT-001` and `INS-EXT-002`: `AGENTS.md` remains a dedicated live input. Each wrapper and file body remains unchanged. Their order intentionally changes from project-first to the accepted global-first order.
- `INS-LEG-005` and `INS-LEG-006`: preferred-tool files remain live compatibility inputs during WP-03. Their managed migration remains owned by WP-07.
- `INS-SYS-003`: the available-skills section keeps its existing renderer and bytes but is now captured in the activation snapshot. Managed prose migration remains owned by WP-05.

## Expert-review correction evidence

Seven independently reported defects were reproduced against the initial candidate and corrected:

1. Direct `Agent::clear` now stages retained-agent composition and Startup Context, persists the complete replacement, and only then swaps live state. Success and failure regressions cover the direct path.
2. Old-session migration now composes and persists against the loaded candidate before live Agent assignment. A damaged store leaves the existing session, provider continuation, and target session unchanged.
3. Successful old-session prompt migration clears both stored and in-memory provider-native continuation before a later request.
4. A four-MiB synthetic prompt proves metadata-only `load_startup_stub` retains agent, dispatch, transition, and skill identity without complete prompt or skill bodies.
5. Applicable invalid or ambiguous project addenda fail composition, while an unrelated damaged addendum remains isolated.
6. Legacy compatibility fallback is limited to true `ResourceNotFound`; invalid and ambiguous managed resources fail, explicit global scope wins, and unqualified same-ID selection re-resolves project specificity.
7. The Harness bridge advertises `initial_agent_selection`. Rust and TypeScript SDKs fail before sending an agent-bearing request to older bridges, while ordinary creation remains compatible.

All regression content is synthetic. No deterministic test judges or locks the compatibility body or approved kernel prose.

The complete full-system prefix changes intentionally because the approved agent-profile kernel is inserted and paired scopes move to global-before-project. Those changes are accepted product semantics, not migration-equality failures.
