# Model roster mechanism verification

This document covers Phase 3 WP-08 mechanisms, not prompt quality or candidate
acceptance. Current behavior and future-owner contracts are in
[`MODEL_ROSTER.md`](../MODEL_ROSTER.md).

Implementation baseline: `3c3576b104540a8b71539c245eaa08a0574d0c17`.
Functional implementation: `8fdb990fa`, `e8024346b`, `4a407e9ea`.

## Requirement mapping

| Requirement | Production path and concrete check |
|---|---|
| Accepted five-alias global roster, descriptions, order, effort, Astra substitution | One-time TOML semantic equality against Mirza's accepted roster. Seed is 953 bytes, SHA-256 `bb5e4c22063aba2b679c94039e0f04c0efb1e4f17ea6fd9832b21b9a2be65117`. No permanent prose snapshot was added. |
| Schema, round trip, no tags/chains, required qualification | `model_roster::resolver::tests::{parse_roundtrip_and_discovery_keep_notes_human_only,validation_is_strict_and_isolates_unrelated_invalid_aliases}` and provider-core `qualified_model::tests` use synthetic content. |
| No project shadowing | The service opens only the global repository. Repository validation rejects project roster files. `roster_validation_participates_in_initialization_commits_retry_and_project_scope` exercises that boundary. |
| Ordered fallback and explicit overrides | `ordered_fallback_and_effort_precedence_are_launch_only`, `explicit_override_bypasses_candidates_not_alias_effort_or_validation`, and `unsupported_effort_continues_and_default_is_provider_owned`. |
| Credential, route, model, effort and complete error accounting | `complete_failure_accounting_and_preview_share_resolution`, typed auth snapshot checks and production-constructor root tests. No missing route inherits a coordinator model. |
| Provider defaults and normalization | Real OpenAI/Anthropic setters and real OpenRouter `max` to `xhigh` normalization through `cli::startup::roster_tests`. Existing provider-core ladders and OpenRouter setter tests remain unchanged in semantics. |
| Native auth, named endpoints, pins, subscription identity | Root tests use the real production registry and constructors with synthetic credentials. The named/Groq collision checks keep the configured endpoint distinct from a built-in profile. OpenRouter runtime tests exercise isolated subscription identity and preserve intentional legacy environment behavior. |
| List, inspect, validation, availability, preview | `ModelRosterService` exposes fresh-source operations. Discovery excludes human-only notes and concrete routes. Preview uses the same candidate resolver as launch. |
| Later edits affect new executions only | `working_edits_and_missing_file_head_recovery_do_not_reresolve_existing_executions` uses a real temporary Git store and retains earlier resolution state while a later working edit selects another model. |
| Durable concrete identity and source-free resume | A reusable `ModelRosterResolution` is written/read through JSON, with provider key, API method, model, effort, requested alias and override provenance. Root tests restore real independent providers from that value without roster source. Ordinary primary Session state is not given an alias. |
| Missing-file async HEAD recovery | The real store test distinguishes an absent working file from invalid TOML, invalid UTF-8, blank content and dangling symlinks. Only absence permits explicit `AllowHeadFallback`. |
| Git publication and retry | The repository test rejects an invalid roster commit and its retry, preserves HEAD, exposes diagnostics and permits an unrelated valid resource edit. Existing full repository/seed-adoption suites retain scoped mutation and deletion guarantees. |
| Primary and existing workflow independence | No current workflow calls the resolver. `ordinary_activation_isolates_invalid_roster_but_selected_sources_and_publication_still_fail` verifies Mirza's approved selected-resource isolation while preserving full explicit initialization validation. Provider constructors retain existing entrypoints and their intentional process environment setup. |
| No model call during resolution | Pure fixtures panic on a model request. Root tests construct actual native/compatible runtimes without calling inference. The resolver returns a prepared provider for the future caller's context/permission/persistence boundary. |
| Future Phase 4/9 interfaces | The current behavior document assigns launch, persistence, tool errors, job/run policy, permissions, quotas, cancellation and retries to their owning future features. No isolated tool or async job was added. |

## Reproduction

Use the repository's coordinated self-development test runner. The principal
commands are:

```sh
scripts/dev_cargo.sh test --profile selfdev -p jcode-provider-core --lib
scripts/dev_cargo.sh test --profile selfdev -p jcode-base --lib model_roster:: -- --test-threads=1
scripts/dev_cargo.sh test --profile selfdev -p jcode-base --lib instruction:: -- --test-threads=1
scripts/dev_cargo.sh test --profile selfdev -p jcode-base --lib provider:: -- --test-threads=1
scripts/dev_cargo.sh test --profile selfdev -p jcode --lib roster_tests:: -- --test-threads=1
scripts/dev_cargo.sh test --profile selfdev -p jcode-provider-openrouter-runtime --lib -- --test-threads=1
scripts/dev_cargo.sh clippy --profile selfdev -p jcode-provider-core -p jcode-base -p jcode-provider-openrouter-runtime -p jcode --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

The root tests deliberately use a dev-only dependency on the already existing
base crate. No new third-party dependency or normal binary dependency was added.
Use `CARGO_INCREMENTAL=0` when disposable Rust caches threaten local disk space.

## Observed evidence and limits, 2026-09-06

- Final root production-registry tests: 2 passed.
- Complete provider-core suite: 142 passed.
- Base provider baseline and integrated route suite: 190 passed.
- Complete instruction/runtime/repository suite passed, including roster
  publication and approved source-isolation checks.
- Complete OpenRouter runtime suite: 121 passed, 1 ignored.
- Strict all-target lint over the changed crates and root, formatting and diff
  checks passed.
- The full serial base suite was actually run: 1,408 passed, 29 failed, 1 ignored.
  Twelve failures expect enabled memory despite the accepted hard disable.
  Three unchanged session-context tests fail on cwd expectations and leave a
  deleted process cwd, causing later skill/transfer fixture failures. The
  aggregate is not reported as passing. The same original memory/cwd failure
  families are documented in prior package reports. Fresh-process reruns passed:
  38 skill tests, 4 transfer tests and all 8 roster tests. The original failing
  session-context, skill and transfer source/test files are unchanged from the
  package baseline, confirming the observed process-cwd cascade.

Native tests use representative catalogs and synthetic credentials, with real
provider construction and effort behavior. They do not claim successful live
inference, available quota, every operating system, or every catalog-only
provider route. The activated baseline catalog recognized all four initial
roster models over OAuth. Its refresh also reported an existing Claude OAuth
refresh-token rejection. Catalog/credential construction is not an inference
health check, and post-launch failure policy remains with the execution owner.

No prompt/skill wording test, automated model-quality evaluation, or paid model
request is part of this verification.
