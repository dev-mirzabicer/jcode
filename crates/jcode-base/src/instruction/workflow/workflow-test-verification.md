Run Jcode's /test verification orchestrator for: {{target}}

Goal: become as sure as reasonably possible before the user checks manually. Do not stop at compile success. Build and execute a verification plan, update todos as needed, and finish with an evidence-backed proof packet.

Required verification layers to consider and run when applicable:
1. Reproduction-first: if this is a bug, create or identify the exact failing repro and prove it now passes.
2. Focused unit tests plus integration tests for real module boundaries.
3. End-to-end/user-flow smoke tests that mirror what the user would manually try.
4. Property-based tests, state-machine/model-based tests, fuzzing, and exhaustive enumeration for small state spaces.
5. Static analysis: formatting, type/check build, clippy/lints, dead code, schema/contract compatibility, secret/security scans, dependency/audit checks when available.
6. Regression strategy: adjacent feature sweep, old-vs-new differential checks, oracle/golden/snapshot comparisons, and metamorphic tests.
7. Robustness: fault injection/chaos for timeouts, network errors, corrupt storage, permission errors, restarts/resume, cancellation, and invalid inputs.
8. Concurrency/race/interrupt/multi-session stress plus soak/flakiness loops where risk exists.
9. Nonfunctional checks: performance/resource regressions, observability logs/events/telemetry, UX/accessibility, and security/safety boundaries.

Final proof packet required:
- Claim verified or not verified.
- Commands/tests/checks actually run and their results.
- E2E/manual-equivalent flows covered.
- Adjacent regressions considered.
- Remaining gaps or untested environments.
- Confidence level and why the user should or should not expect to hit another obvious error.