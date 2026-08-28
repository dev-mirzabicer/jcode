# Context Control Acceptance Ledger

This is the durable requirement-to-evidence ledger for Jcode's user-controlled
provider-context subsystem. It complements the current-behavior reference in
[`../CONTEXT_CONTROL.md`](../CONTEXT_CONTROL.md).

The ledger serves four purposes:

1. Map every original and post-deployment product outcome to documentation.
2. Map every outcome to implementation and acceptance evidence.
3. Define reproducible performance, provider, privacy, migration, runtime, and
   repository gates.
4. Prevent a future refactor from weakening accepted behavior because its rationale or
   validation boundary was forgotten.

## Acceptance identity

Phase 17 began from:

```text
branch: local/gpt56-sol-372k
HEAD: d8b1b2c739fa4b8feff992c85c66ce8e72be8fbc
subject: provider: add GPT-5.6 Sol 1M profile
tree: 90b201d922dbfccb8e42e1d2f8e576211f4b0c02
```

Accepted subsystem chronology before Phase 17:

```text
ba3ce3000 context: remove automatic and native compaction machinery
a8b539480 context: make context operations reliable across repeated use
2cd687050 context: make curator preparation atomic and configurable
bc1e74d1a tui: redesign curator preparation workflow
e3681769d context: expose coverage and file evidence
d8b1b2c73 provider: add GPT-5.6 Sol 1M profile
```

Phase 17's immutable implementation boundary is:

```text
commit: 0ca9bc6e1f1d739a654d73174e06ffa52760abe0
subject: context: complete final system acceptance
staged/committed patch SHA-256: 1b576cda4607e28dce8ba38343d6d79f53964a48f12d18b19e5235d63e581958
```

The reviewed staged patch and `git show --format= --binary` committed patch were proven
byte-identical. A documentation-only signoff commit records this immutable implementation
identity; its own hash is intentionally recorded in the external chronological completion
handoff rather than self-referenced inside its tree.

## Protected baseline

The Phase 17 baseline contained only five pre-existing protected paths:

```text
 M crates/jcode-desktop2/src/scene.rs
 M crates/jcode-desktop2/src/tests/actions.rs
?? .DS_Store
?? assets/.DS_Store
?? assets/demos/.DS_Store
```

Protected file hashes at baseline:

```text
crates/jcode-desktop2/src/scene.rs
6c237c3cb909d74dd52bdfeea93521597e357563914fbd55e62bce253a259743

crates/jcode-desktop2/src/tests/actions.rs
6eb4999a7fc30641a14283074fccb97eabb3c1c22a8e4bcf7249650759a19903
```

Phase 17 must not edit, delete, stage, format, or commit these paths.

Baseline runtime evidence:

```text
running: jcode v0.75.32-dev (d8b1b2c73, dirty)
current/shared/canary: d8b1b2c73-dirty-af814aea5591
canary: passed
scheduled tasks: none
target/selfdev/incremental: 0B
available data-volume disk: approximately 18 GiB
```

`~/.jcode/testers.json` contained one stale record for PID 55850; no such process was alive.
The registry was not rewritten.

### Recovered-session field evidence

Before Phase 17, one production session had an intact transcript but a missing current
`context_view`. Recovery used exact pre-wipe persistence evidence rather than regenerated
content:

```text
session: session_rabbit_1787445699864_42942623aac97fd5
authoritative messages: 668
message-array SHA-256: 954cc27c497205962dafcc20fe4022f4fca2d24edea03ea7c8bde5b840fde4b2
raw provider-token estimate before recovery: 686,623
recovered context revision: 5
recovered transactions: 5
context-view SHA-256: 2df79379bfacb8767bccef8fc99f16f3708985d1078a0e6deeaad9797f2ecc07
projected provider-token estimate after recovery: 310,658
strict projection warnings: 0
```

Only `context_view` was inserted atomically. The authoritative message array remained
byte-for-byte/equality verified, and the user subsequently opened and continued the
session successfully.

The exact writer that removed the state was not proven. An older client/tester predating
later lifecycle repairs is a compatibility hypothesis, not an established cause. This
evidence therefore strengthens preservation and migration acceptance without authorizing
speculative repairs or causal claims.

## Requirement-to-document matrix

| Requirement family | Authoritative documentation |
|---|---|
| One transcript and reversible projection | `CONTEXT_CONTROL.md`: Core model; Non-negotiable invariants |
| Range summaries, reasoning suppression, distillation | Context Editor workflow; Persisted operations |
| Stable targets, hashes, closure, provider validation | Range selection; Atomic commit; Provider validation |
| Multiple ranges and one atomic transition | Core model; One task per call; Atomic commit |
| Strict below-20-percent distillation and default selection | Tool-result distillation |
| Undo, restore, reapply, provenance | Active coverage; Persisted operations; Session lifecycle |
| No interactive automatic mutation | Non-negotiable invariants; Context pressure |
| Prompt/attachment preservation and no auto-resend | Blocked interactive requests |
| Explicit unattended emergency authorization | Explicit unattended emergency authorization |
| Cache-prefix economics and one reset | Cache economics and one-transition behavior |
| Curator isolation and coding-provider separation | Curator architecture and user control |
| One semantic task per independent call | One task per call |
| Approved role prompts and complete source | Complete-source guarantee; Prompt transparency |
| Route/model/effort and per-run instructions | Per-run settings and saved defaults |
| Durable simple curator defaults | Per-run settings and saved defaults |
| Repeated suppression idempotence and no-op | Replayed-reasoning suppression |
| Direction-independent user ranges | Range selection and structural closure |
| Active coverage, paging, and provenance | Active-operation coverage and provenance |
| Changed/read/searched evidence and uncertainty | File evidence and summary provenance |
| Migration and obsolete config compatibility | Legacy migration and compatibility |
| Provider-specific request shapes and caveats | Provider validation and caveats |
| Reload, split, transfer, rewind, clear, switch, remote | Session lifecycle |
| Privacy, redaction, safe logs/debug | Privacy, export, and diagnostics |
| GPT-5.6 Sol base and explicit 1M profiles | GPT-5.6 Sol context profiles |
| Source ownership and future maintainer guidance | Architecture and source ownership; Maintainer checklist |
| Final performance, provider, repository, runtime acceptance | This acceptance ledger |

## Requirement-to-acceptance matrix

A row is complete only when its stated check has an observed result. Aggregate test counts
without requirement-level mapping are not sufficient.

### Authoritative state and projection

| Outcome | Implementation anchors | Required acceptance |
|---|---|---|
| Raw transcript remains byte-for-byte unchanged | `jcode-context-core/src/projection.rs`; `jcode-base::Session` | Projection, apply, revert, reapply, failure, reload, rewind, and migration fixtures compare serialized raw messages before/after. |
| Provider view is deterministic and provider neutral until final builder | `project_context`; `StoredContextViewState` | Combined-operation projection is stable for identical input and source maps remain chronological. |
| Stable targets never guess | `jcode-context-core/src/target.rs` | Reordering with semantic identity succeeds; content/hash change, duplicate IDs, missing blocks, and stale drafts fail closed. |
| Structural closure never splits logical groups | `closure.rs`; range preview service | Forward/backward tool, parallel call, result-image, thought-signature, and active-summary boundary fixtures produce the same exact closed interval. |
| Active summaries do not overlap | projection/state validation | Overlap fails before persistence; reviewed shadowing is represented explicitly. |
| Distillation changes only result content | projection | `tool_use_id` and error state remain identical; empty markers still preserve the result block. |

### Transaction atomicity and repeated use

| Outcome | Implementation anchors | Required acceptance |
|---|---|---|
| Multiple operations apply in one revision | `context/commit.rs` | Multi-range, reasoning, and selected distillation operations persist once and emit one context reset. |
| Failure cannot activate partial work | draft/curator/commit services | Missing artifact, invalid response, stale identity, provider rejection, cancellation, timeout, persistence failure, and client race leave either the complete prior state or one complete commit. |
| Revert and reapply preserve provenance | `context/history.rs`; append-only status events | Each successful transition creates one revision/reset; failed transitions restore exact state without reset. |
| Repeated suppression is idempotent | `context/draft.rs` target subtraction | Active, reverted, reapplied, and transcript-invalidated histories select only newly eligible targets. |
| Fully redundant suppression is a no-op | draft/review/apply gating | Visible no-change review cannot queue apply and causes zero persistence, revision, cache, or continuation changes. |
| User range direction is irrelevant | editor/protocol normalization plus closure | Forward and backward summary and manual-reasoning selections produce equal canonical previews in local and remote paths. |

### Curator isolation, completeness, and control

| Outcome | Implementation anchors | Required acceptance |
|---|---|---|
| One semantic task per call | `context/curator.rs::build_context_curator_plan` | Three ranges and five tool candidates produce exactly eight independent provider forks/calls. |
| Calls contain no selected sibling source | per-task `ContextCuratorTask` scope | Captured requests prove no range/tool source or task-specific instruction leaks to another task. |
| Roles use separate exact contracts | range/distiller prompts and schemas | Range prompt includes the approved verbatim base; distiller receives its purpose-specific contract. |
| Complete source is never truncated | `build_task_request`; `prepare_task_call` | Full source and image blocks reach the provider; byte/token/image limit failures occur before a provider call and leave active context unchanged. |
| Exact reviewed plan equals generated plan | plan preview fingerprint and draft request identity | Route, effort, prompts, response schema, source scope, evidence, instructions, transcript digest, revision, and plan fingerprint mismatches fail stale. |
| Per-run settings remain ephemeral | curator workspace/runtime | Using a temporary route/model/effort and instructions does not alter durable config. |
| Explicit saved defaults are durable and reversible | `Config::set_context_curator`; workspace reducer | Save survives restart, preserves unrelated TOML/comments/unknown keys, removes only cleared owned keys, and reports write failure. |
| Curator administration never enters coding context | independent provider forks and no transcript writes | A recording main provider sees no curator prompt, source administration, or maintenance instruction. |
| Usage/cost is attributable per call | `StoredContextCuratorUsage` | Input, output, cache read/creation, role, artifact, route, prompt version, and exact cost when available are separate from coding-turn token usage. |

### Context Editor, coverage, evidence, and protocol

| Outcome | Implementation anchors | Required acceptance |
|---|---|---|
| Local and remote editor semantics agree | typed protocol and common reducer | Command, selection, preview, plan, generation, apply, history, provenance, stale state, and reconnect scenarios agree. |
| Keyboard and mouse actions agree | editor state machine/hit regions | Every enabled toolbar action matches its key path; disabled actions are inert. |
| Narrow layout remains complete | responsive editor/workspace rendering | 72x24 or smaller acceptance keeps focus, actions, disclosure, scrolling, and confirmation reachable. |
| Coverage is pagination safe | snapshot computes global coverage before slicing | Prefix, middle, suffix, single, adjacent, disjoint, interior page, both-direction continuation, and search-gap rails are exact. |
| Active and staged operations differ visibly | semantic rails and markers | `S` rails/R/D cannot be confused with staged `Σ`; trace-only reasoning never displays token-saving `R`. |
| Provenance reaches exact owner and returns exactly | transaction-detail correlation and origin | Same-transaction repeat, stable block return, missing-block refusal, session/revision invalidation, and no fallback retargeting pass. |
| File evidence categories remain distinct | `context/change_digest.rs` | Successful structured tools, failed/missing/duplicate results, patches, batch aliases, shell, unknown tools, search/outline, browser, and path normalization map honestly. |
| Evidence bounds do not imply false completeness | persisted limits and validation | Omitted paths/warnings add aggregate uncertainty; malformed complete/incomplete state is rejected. |
| Protocol is bounded and correlated | `jcode-protocol/src/context.rs` | DTO round-trips, defaults, max page/detail/instruction/event bounds, request IDs, draft IDs, revision/digest, and stale event suppression pass. |

### Pressure, emergency recovery, and prompt preservation

| Outcome | Implementation anchors | Required acceptance |
|---|---|---|
| Safe budget uses provider semantics | `ContextRequestBudget`; `context/preflight.rs` | Input-only, combined actual output cap, and unknown semantics produce exact budgets without guessed reserve. |
| Notice/urgent are non-mutating | `pressure.rs`; TUI banner state | Threshold boundaries update UI state and never append warning text or create transactions. |
| Preflight block preserves one prompt | agent rollback plus TUI correlated restoration | Multiline, Unicode, pasted content, images, occupied composer, and remote-disconnect cases preserve exact input and produce one user turn on manual resend. |
| Provider context rejection before output preserves prompt | provider-size rejection path | No hard compaction or automatic retry occurs. |
| Rejection after output preserves partial authoritative work | turn checkpoint path | Partial output remains and the turn is not falsely restored as unsent. |
| HTTP 413 preserves images and payload | payload-pressure path | Stored images remain byte-identical; no stripping or automatic resend occurs. |
| Emergency policy defaults to block | persisted policy, server, schedule parser | New sessions/tasks, omitted policy, restart, and unrelated tasks remain blocked. |
| Authorization is exact and isolated | `StoredUnattendedContextAuthorization` | Per-task copy survives restart and later session policy changes; one task never authorizes another. |
| Authorized recovery is one audited transaction and retry | `agent/emergency.rs`; commit audit | Pending prompt, recent turns, active tool pair, raw transcript, operation order, target headroom, transaction ID, and retry result are proven. |
| Insufficient safe reduction blocks | emergency failure path | No partial transaction, raw dropping, deterministic truncation, image removal, second surgery, or second retry occurs. |

### Persistence, migration, lifecycle, and privacy

| Outcome | Implementation anchors | Required acceptance |
|---|---|---|
| Context state persists everywhere authoritative | session snapshot/journal/startup/remote metadata | Active/reverted/reapplied/evidence/emergency/default states round-trip through save, reload, stub, and remote startup. |
| Legacy text summary becomes reversible | `Session::migrate_legacy_compaction_state` | Exact summary and coverage become one deterministic migration transaction; save/reload is idempotent and loses no source. |
| Encrypted-only/invalid legacy state restores raw | migration outcome paths | No fake summary is created; continuation clears; oversize raw context blocks safely. |
| Obsolete config and wire controls cannot execute | config compatibility and typed rejection | Old keys deserialize permissively but never serialize/fingerprint; old requests return migration errors and mutate nothing. |
| Split/child continuation clones independently | child construction sites | Child starts with exact state; child revert leaves parent unchanged. |
| Transfer is authoritative and self-contained | transfer handoff builder | Child has a raw handoff message and default context state. |
| Rewind invalidates only stale transactions | lifecycle reconciliation | Valid old transforms remain; removed-source transforms receive one invalidation revision; undo restores exact prior state. |
| Clear and provider/model switch reset correctly | agent lifecycle hooks | Clear empties context state; switch revalidates projection and invalidates continuation without changing raw messages. |
| Redacted export contains no generated secret | `Session::redacted_for_export` | Secrets in summaries, digests, structured evidence, replacements, rationales, instructions, and authorization/provider errors are absent; original session remains unchanged and valid. |
| Safe logs/debug contain no source | editor/server debug payloads | Mechanical scans and fixtures expose only IDs/counts/status/tokens, never raw curator or generated content. |
| Recovered-session evidence is represented honestly | Phase 17 field record | Acceptance records exact message/context hashes and successful subsequent use without claiming an unproven writer/root cause. |

### Provider and model-profile boundaries

| Outcome | Implementation anchors | Required acceptance |
|---|---|---|
| Provider builder support fails closed | provider runtime adapters | Every production provider either passes its real builder or returns a precise unsupported result before commit. |
| OpenAI historical edit starts fresh | continuation invalidation hook | `previous_response_id` is absent after revision; full projected input is sent; incremental continuation resumes only after a successful fresh baseline. |
| Provider reasoning semantics are exact | common validation report plus adapters | Only generic, Anthropic, or OpenAI replay blocks matching the active family are removable; trace and thought signatures never are. |
| Base Sol remains default | curated model order and auth preference | Catalog order, onboarding, login, and global default choose `gpt-5.6-sol`, not `[1m]`. |
| Sol 1M remains distinct and explicit | profile identity/model resolution | Picker/persistence retain `gpt-5.6-sol[1m]`; context is 1,000,000 and OAuth safe budget 995,904. |
| Both Sol profiles share upstream model | final wire normalization | Requests strip `[1m]` and send `gpt-5.6-sol`; no unsupported context/compaction field is added. |
| Route boundaries remain honest | catalog route construction | Direct OpenAI routes expose 1M when base Sol is available; OpenRouter gets no fabricated `[1m]` upstream model; managed subscription catalog remains unchanged. |
| Long-input pricing remains tier aware | provider pricing and context economics | Direct API whole-request estimates above 272K select the long band; subscription route shows no fake per-request dollars. |

## Representative performance and memory acceptance

### Fixture

The manual performance fixture must use the real public/session/editor code paths and include:

- 10,000 authoritative stored messages,
- at least 500 complete tool call/result pairs,
- multiple active range, reasoning, and distillation transactions,
- large tool-result text,
- image payloads in source messages,
- a 1,000-row maximum Context Editor page,
- complete global summary coverage before page slicing,
- and representative search terms that match none, few, and many rows.

Image payloads must not appear in `ContextEditorMessage`; rows expose only bounded preview and
`has_image_payload` metadata. Message detail remains separately chunked.

### Measurements

Record release-independent observations for:

1. Initial pure projection.
2. First `Session::projected_provider_messages` cache population.
3. Unchanged cache reuse.
4. One-message append fast path with prefix-hash preservation.
5. Full Context Editor snapshot construction.
6. Snapshot pagination to the maximum 1,000-row page.
7. Search/filter and cursor/scroll loops on the loaded page.
8. Context-revision commit rebuild.
9. Serialized `StoredContextViewState` size.
10. Session memory profile before projection, after projection, and after cache release.
11. Snapshot and page serialized payload size.

Report warmup count, sample count, median, minimum, maximum, fixture counts/sizes, source
commit, profile, operating system, architecture, and whether other Jcode processes were
stopped. Do not add unstable wall-clock pass/fail thresholds to normal CI.

The measurements are ignored tests so ordinary suites compile them but never execute the
resource-intensive workload. After obtaining approval for an isolated validation window,
run them explicitly and capture their JSON reports:

```bash
scripts/dev_cargo.sh test --profile selfdev \
  -p jcode-app-core --lib \
  phase17_representative_large_session_projection_snapshot_cache_and_memory \
  -- --ignored --nocapture --test-threads=1

scripts/dev_cargo.sh test --profile selfdev \
  -p jcode-tui --lib \
  phase17_representative_context_editor_search_and_scroll \
  -- --ignored --nocapture --test-threads=1
```

The first report is prefixed `PHASE17_CONTEXT_PERFORMANCE=`. The second is prefixed
`PHASE17_CONTEXT_EDITOR_PERFORMANCE=`.

### Phase 17 observed measurements

Measured on 2026-08-28 with all other Jcode sessions closed, macOS/aarch64, the `selfdev`
profile, two warmups, seven recorded samples, and source identity
`v0.75.32-dev (d8b1b2c73, dirty)`:

| Operation | Median | Minimum | Maximum |
|---|---:|---:|---:|
| Pure projection of 10,000 messages | 55.329 ms | 54.938 ms | 56.234 ms |
| First projected-session cache population | 187.242 ms | 186.779 ms | 187.938 ms |
| Unchanged projected-cache reuse | 41 ns | 0 ns | 41 ns |
| One-message append fast path | 28.750 µs | 23.625 µs | 37.417 µs |
| Context-revision cache rebuild | 195.009 ms | 192.284 ms | 195.668 ms |
| Full Context Editor snapshot | 220.714 ms | 218.783 ms | 222.725 ms |
| Maximum 1,000-row page extraction | 1.274 ms | 1.235 ms | 1.343 ms |
| Search with no match on 1,000 loaded rows | 0.408 ms | 0.384 ms | 0.579 ms |
| Search with one match | 0.416 ms | 0.409 ms | 0.443 ms |
| Search with 100 matches | 0.476 ms | 0.464 ms | 0.596 ms |
| 500 cursor moves down and 500 up | 72.455 ms | 72.279 ms | 73.132 ms |

Fixture identity and size:

```text
authoritative messages: 10,000
complete tool pairs: 500
active transactions: 7
active range summaries: 5
active reasoning suppressions: 1
active tool-result distillations: 1
image messages: 10
serialized context state: 6,912 bytes
serialized full snapshot: 5,149,920 bytes
serialized 1,000-row page: 551,185 bytes
```

The canonical session occupied 5,171,330 JSON-accounted bytes before projection. The
projected cache added 4,107,402 bytes and held 8,005 projected messages. Releasing provider
caches returned projected-cache message count, JSON bytes, tool-result bytes, and large-blob
bytes to zero while leaving canonical messages and context state unchanged.

The fixtures also asserted strict state validity, complete tool-pair structure, global
summary coverage before page slicing, bounded character previews, image-payload exclusion
from snapshot JSON, and exact cache release. Neither measurement establishes a stable CI
latency threshold; the observations are a reproducible Phase 17 baseline.

Focused validation on the same source passed:

```text
jcode-app-core context suite: 96 passed, 0 failed, 1 ignored measurement
jcode-tui Context Editor suite: 56 passed, 0 failed, 1 ignored measurement
representative context measurement: 1 passed, 0 failed
representative editor measurement: 1 passed, 0 failed
format: passed
whitespace: passed
```

### Phase 17 deterministic package acceptance

The final ambient-development-environment matrix ran all 26 directly affected contract,
provider, and integration library suites sequentially with one test thread per package:

```text
source identity: v0.75.32-dev (d8b1b2c73, dirty)
task: 256064a82h
duration: 399.333 seconds
packages: 26 passed, 0 failed
tests: 5,754 passed, 0 failed, 49 ignored
jcode-base: 1,291 passed, 0 failed, 1 ignored
jcode-app-core: 1,326 passed, 0 failed, 25 ignored
jcode-tui: 2,264 passed, 0 failed, 18 ignored
```

Exact per-package logs and the machine-readable summary are under:

```text
~/.jcode/scratch/phase17-validation/
  full-deterministic-ambient-postfix-20260828T1158Z/
```

The broad run exposed and closed two pre-existing provider-test integration seams rather than
masking them with a sanitized shell:

1. Anthropic server guidance such as `Please use Opus 4.8.` treated the decimal point as a
   sentence boundary, reduced the hint to `Opus 4`, and could select Opus 4.5 on a cold
   catalog. The parser now preserves decimal version separators. Its exact regression and
   complete 54-test runtime suite passed under an isolated cold home.
2. The Sol OAuth-profile and named OpenAI-compatible effort tests inherited ambient runtime
   route or user-effort configuration. Each test now pins the exact configuration it proves.
   The focused tests and complete OpenAI/OpenRouter runtime suites passed under the actual
   developer environment.

No production provider capability was weakened to make a test pass.

The root `jcode` library was then validated separately because it is not one of the 26
contract/provider/integration packages in that matrix:

```text
jcode root library: 217 passed, 0 failed, 0 ignored
strict root all-target Clippy: passed with -D warnings
```

That gate exposed and mechanically closed three unrelated pre-existing root warnings: a
macOS-only unused Linux debug-client helper, nested menu-bar PID guards, and a retry-counter
modulo expression. Linux behavior, menu-bar behavior, and notification retry cadence were not
changed.

#### Enumerated ignored tests

The 49 ignored tests are deliberate credentialed, GUI, network, local-corpus, reporting, or
manual-measurement harnesses. They are not hidden in the aggregate:

- `jcode-provider-anthropic-runtime`: `live_anthropic_reasoning_smoke`.
- `jcode-provider-openai-runtime`: `live_openai_catalog_lists_gpt_5_4_family`,
  `live_openai_gpt_5_4_and_fast_requests_succeed`.
- `jcode-provider-openrouter-runtime`: `live_openrouter_unified_reasoning_smoke`.
- `jcode-provider-bedrock`: `bedrock_live_smoke_test`.
- `jcode-base`: `live_keychain_native_credentials_detected_and_parsed`.
- `jcode-app-core`: `test_relay_live_roundtrip`, the Phase 17 representative context
  measurement, eight `tool::computer::coverage_tests` live cases, ten `tool::computer::tests`
  GUI/permission cases, `bench_real_session_search_corpus`,
  `print_tool_definition_token_report`, `corpus_report`, `dump_outputs`, and
  `live_fetch_latest_release_uses_auth`.
- `jcode-tui`: `dump_pretty_model_names_for_manual_audit`,
  `onboarding_suggestion_scan_cost`, `measure_slash_palette_frame_cost`, the Phase 17
  representative editor measurement, four info-widget demo/report cases, nine real-resume
  benchmark cases, and `print_measured_palette_topology`.

Both Phase 17 measurements were run explicitly and passed; their observations are recorded
above. Credentialed and external-environment cases remain governed by their dedicated gates.

### Qualitative gates

- Projection remains linear after target/range indexing.
- Unchanged and append-only calls do not rebuild old projected history.
- A context revision rebuilds once and replaces cache accounting rather than accumulating.
- Context-state storage is proportional to artifacts and targets, not duplicated source.
- Initial editor payload is paged, bounded, and contains no base64 image body.
- Search and scrolling operate only on the loaded page and retain stable selections.
- Releasing provider-message caches returns their memory-profile counts and JSON bytes to zero.

## External provider validation and user ratification

Live probes require explicit user authorization and existing credentials. They use only
synthetic non-sensitive content and must never print tokens, cookies, API keys, encrypted
reasoning, or raw secret-bearing payloads.

| Probe | Minimum scenario | Final Phase 17 status |
|---|---|---|
| Anthropic | Remove historical signed thinking around a tool-use turn on a current thinking-capable model. | No new credentialed probe. Live Anthropic behavior is not claimed; the production formatter validation remains fail-closed. |
| OpenAI Responses stateless | Remove historical reasoning from text and tool-call turns and send a fresh projected request. | No new quota-consuming synthetic probe. The subsystem received extensive real OpenAI production use as described below. |
| OpenAI persistent WebSocket | Establish continuation, apply a historical transform, prove reset/full request, then prove continuation can resume. | No new quota-consuming synthetic probe. Deterministic runtime tests prove reset/full-request behavior; exact external transport behavior remains bounded by safe provider rejection. |
| OpenRouter-compatible | Remove generic replay reasoning on a route that supports it. | No new credentialed probe. Live route behavior is not claimed; exact model-dependent builder validation remains fail-closed. |
| Gemini | Summarize an older tool-call range and preserve thought signatures in later valid calls. | No new credentialed probe. Live Gemini behavior is not claimed; thought signatures remain structurally protected and builder validated. |
| GPT-5.6 Sol 1M profile | Select the explicit profile through OpenAI OAuth and make a real request with the 1M budget. | Accepted through sustained real OpenAI OAuth use before and during Phase 17. |

If a route is unavailable, mark that probe externally blocked. Do not generalize from mocks
or silently enable an unverified operation. Builder validation remains the safe enforced
boundary.

On 2026-08-28 Mirza explicitly ratified external provider validation as **not a Phase 17
completion blocker**. This was an informed decision based on extensive production use of the
implemented subsystem with OpenAI across approximately 20–30 sessions, including every
Context Editor operation. Available OpenAI subscription quota was intentionally not spent on
duplicative synthetic requests. If a provider-specific field problem appears later, it can be
reproduced in a dedicated session without weakening transcript preservation or fail-closed
validation in the meantime.

This ratification is not a claim that Anthropic, OpenRouter, or Gemini live APIs were exercised
in Phase 17. It accepts the existing boundary: real production request builders validate before
commit, unsupported shapes fail closed, provider rejection cannot destructively rewrite the
transcript, and deterministic provider/runtime suites remain green.

## Final command gates

Tests, lint, benchmarks, and static checks may run autonomously in this Phase 17 session.
Every build or build/reload action still requires explicit user approval because live Jcode
processes may be affected. Record the exact command, source identity, exit status, pass/fail/
ignored counts, duration when useful, and any environmental warning.

### Static and repository checks

- `cargo fmt --all -- --check`
- `git diff --check`
- strict Clippy for every changed or directly affected package with `--all-targets --no-deps -- -D warnings`
- Cargo metadata/dependency inventory proving no obsolete compaction crate or dependency remains
- repository searches for active automatic compaction, native `context_management`, hard compaction, destructive image stripping, deterministic curator truncation, and cross-task curator batching
- documentation-link and terminology review
- secret, raw-curator-payload, unsafe, lint-suppression, and TODO/FIXME/HACK scan of the final patch

### Focused behavioral checks

Run direct filters covering:

- pure projection/closure/target/validation/economics/pressure,
- context persistence, migration, redaction, cache, and memory profiling,
- draft/curator/commit/history/snapshot/file-evidence/preflight/emergency services,
- config defaults and durable save,
- protocol DTOs and stale correlation,
- OpenAI/Anthropic/OpenRouter/Gemini/Bedrock/Copilot/Cursor/Antigravity builder adapters,
- Context Editor reducer, workspace, coverage, provenance, narrow layout, prompt restoration,
- GPT-5.6 Sol profile resolution, route identity, default ranking, wire model, budget, effort,
  pricing, and picker display,
- and the ignored representative performance measurement.

### Broad package checks

At minimum, run full deterministic library suites for directly affected contracts and the
three principal integration crates:

- `jcode-session-types`
- `jcode-context-core`
- `jcode-protocol`
- `jcode-config-types`
- `jcode-provider-core`
- `jcode-base`
- `jcode-app-core`
- `jcode-tui`
- directly validated provider runtime packages

Any intentionally ignored credential or environment tests must be enumerated rather than
hidden in an aggregate count.

### Build, runtime, and visual checks

- coordinated TUI build,
- coordinated build/reload only after explicit approval,
- canary acceptance,
- post-reload version/source identity,
- wide and 72x24 Context Editor frames,
- editing, closure, curator exact calls, progress, final review, coverage, provenance,
  history, emergency policy, urgent/blocked pressure, prompt restoration, and unknown-price
  states,
- local and remote reducer paths,
- and a hands-on end-to-end explicit context transaction in the reloaded TUI.

## Final acceptance record

Complete this section before Phase 17 delivery.

```text
Phase 17 implementation commit: 0ca9bc6e1f1d739a654d73174e06ffa52760abe0
Reviewed staged patch SHA-256: 1b576cda4607e28dce8ba38343d6d79f53964a48f12d18b19e5235d63e581958
Committed patch SHA-256: 1b576cda4607e28dce8ba38343d6d79f53964a48f12d18b19e5235d63e581958; byte-identical to staged patch
Format: passed
Whitespace: passed
Strict lint: passed for every directly affected package/provider runtime and root jcode, all targets, -D warnings
Focused tests: 154 passed, 0 failed; two measurements passed when explicitly selected
Broad tests: 5,971 passed, 0 failed, 49 enumerated ignored across 27 package suites
Performance fixture: passed; exact observations recorded above
Credentialed probes: user-ratified as non-blocking; extensive OpenAI production use accepted; no new quota-consuming probe; other provider live behavior not claimed
Build: exact implementation commit built successfully at 2026-08-28T16:19:06Z
Reload: SocketReady at 2026-08-28T16:19:07.411955Z; request reload_1787933947192_9068933112790820627
Canary: 0ca9bc6e1-dirty-da87b673964a passed; current/shared runtime matches
Visual acceptance: complete >=35-fixture matrix plus exact wide, 72x24/extreme-narrow, coverage, provenance, evidence, curator, critical-status, pressure, privacy, and toolbar-overlap checks passed; live final-build tester fixture/state paths also passed
Hands-on acceptance: Mirza ratified extensive real OpenAI use across approximately 20–30 sessions, including every Context Editor feature; no production context-control behavior changed after that accepted use
Protected path hashes: scene.rs 6c237c3cb909d74dd52bdfeea93521597e357563914fbd55e62bce253a259743; actions.rs 6eb4999a7fc30641a14283074fccb97eabb3c1c22a8e4bcf7249650759a19903; unchanged
Final repository status: only the five pre-existing protected desktop/.DS_Store paths remain; temporary testers and debug-control toggle removed
```

No completion claim is valid while a required row remains `pending`, unless the row is
explicitly marked externally blocked with its capability still disabled or bounded safely.
