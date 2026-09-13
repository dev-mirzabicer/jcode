# Full-suite failure dispositions from WP-02

**Status:** Audited 2026-09-13. This accounts for failed historical runs, not a
claim that a new monolithic run passed. WP-02 candidate acceptance remains separate.

The complete [per-test inventory](EXECUTION_FAILURE_TRIAGE.json) names all 135
failures in the two bounded runs and gives either exact later passing evidence or
the source-owned policy that makes the old expectation inapplicable by default.

| Original run | Repaired and reverified | Expected disabled Swarm/workflow expectations | Expected disabled memory expectations |
|---|---:|---:|---:|
| app-core: 1,398 passed / 119 failed / 25 ignored | 46 | 70 | 3 |
| base: 1,567 passed / 16 failed / 3 ignored | 1 | 3 | 12 |

## What was fixed in WP-02, not deferred

- The unbounded Swarm test receive, missing isolated feature preconditions, leaked
  instruction-store registries, panic-unsafe cwd changes, and simulated reload
  writes without a private home. No production retirement guard was relaxed.
- Two actual delivery defects found while investigating reload failures: a closed
  compatibility channel could hide a real terminal result, and delayed completion
  could look up the same run ID in the wrong current store. Their red tests and
  corrected ownership tests remain in the repository.
- The final active caller failures: notification delivery, explicit resume,
  authoritative transfer, and simulated debug reload. Three passed independently
  before fixture correction. All four now have private home/runtime setup and
  pass with their original deadlines and behavior assertions. Strict lint passes.

The `startup_recovery_*headless*` fixtures in the inventory are specifically
Swarm-snapshot restoration fixtures. Their source calls `persist_swarm_state_snapshot`
and `load_runtime_state`, both deliberately disabled by accepted WP-01. They are
not evidence that native command-worker reload or ordinary parent recovery fails.
Those live paths have separate actual-process and native daemon-restart tests.
Similarly, the old workflow split test asks for the retired review-startup workflow,
not ordinary split/transfer. D-27 controls that expected rejection.

## What the expected-failure category means

These tests expect a feature's **enabled** behavior while the downstream default
intentionally disables it. Source guards and actual diagnostics establish the
mismatch. It is not a passing test, proof that the dormant implementation works,
or permission to reactivate memory/Swarm. No current product defect is deferred
merely by assigning a failed test to this category.

The two originally failing integrated reload tests also encountered global
controls left by earlier fault-injection fixtures whose temporary stores had been
deleted. The isolated reload tests and refreshed native workflows pass. Production
reload must continue to refuse uncertain owners rather than deleting those controls
to silence a suite. A successful targeted rerun does not retroactively turn the
original aggregate into a green run.

## Explicit non-urgent follow-up

**Owner:** Phase 10 verification/maintenance tooling. Phase 4 WP-06 and independent
closeout must retain this accounting in their combined evidence and flag any new
active-path regression immediately.

1. Separate dormant enabled-path fixtures from the default downstream matrix using
   explicit isolated feature prerequisites and a named opt-in suite. Keep negative
   hard-disable tests in the normal matrix. Do not add blanket ignores or remove
   valid assertions just to report green.
2. Give synthetic background-control fixtures a complete teardown before deleting
   their temporary stores, including deliberate persistence-failure cases. Never
   change production uncertainty handling to compensate for test teardown.
3. Make private `HOME`, `JCODE_HOME`, and `JCODE_RUNTIME_DIR`, panic-safe restoration,
   bounded waits, progress reporting, and live-state preservation the standard
   broad-suite harness. Re-run the combined matrix after those fixture changes.

Completion evidence for this follow-up must distinguish enabled-fixture, disabled-
policy, and integrated results, account for every listed test, show no leaked
process/lease or real-home mutation, and preserve watchdog failures as failures.
This is test-harness maintenance, not a new product capability or a waiver of any
unfinished WP-02 requirement.

Private original/rerun logs are indexed by the WP-02 `LEDGER.md` under the program's
scratch evidence root. The JSON references log paths relative to that root and
contains no copied tool bodies, credentials, or live configuration contents.
