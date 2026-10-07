# Durable primary input and location acceptance

This guide covers SP-58-C01/WP-04, R12–R14, including the approved adoption of
stable delivery identity in the existing TUI. It is a verification map, not a
claim that the later workspace-management UI, permission policy or service
supervision is complete. Current contracts are in
[PRIMARY_INPUT_LOCATION.md](../PRIMARY_INPUT_LOCATION.md).

## Requirement evidence

| Requirement | Production owner and concrete route |
|---|---|
| R12 original input, exact duplicate/conflict, private persistence | `PrimaryInputStore` and Session input receipts. `primary_input::tests` verifies exact Unicode/images, same-ID conflict, before/after-checkpoint reconciliation, strict corrupt/missing-state handling, original-content read, cancelled tombstones and all-target cancellation validation. |
| R12 idle/busy/detached delivery | `PrimaryHost` admission and retained drain plus the existing Agent loop. `durable_primary_input_detached_replay_and_busy_boundary`, notification, scheduled-delivery and Jade relay tests cover their actual callers. Deferred NextTurn does not hide eligible SafeBoundary/urgent input. |
| R12 transport loss and process recovery | `verify_primary_input_location.py` drops a reply, retries the same UUID, restarts and kills only its own daemon while input is committed/queued. It checks canonical receipts and one-effect files, not only a successful process exit. |
| R12 complete TUI intent | `verify_primary_tui_input.py` drives real keys through a private lossy proxy, replaces the client, compares original UUID/payload copies, checks image bytes, queue mode, exact Ctrl-Up cancellation and two completed native execution records. `durable_client_journal` additionally verifies owner leases, known-failure retry provenance, late receipts and corruption. |
| R12 compatibility and consumer contracts | Harness API v1.8 and both SDKs negotiate actual capabilities, preserve target/UUID/operation identity and reject mismatched replies. The native core fixture exercises the actual bridge and TypeScript SDK without creating a provisional Session. Legacy numeric transport correlation is not advertised as cross-connection idempotence. |
| R13 atomic move and non-waking idle application | Workspace location journal plus one Session checkpoint for binding and notice. Base `workspace::primary_controls::tests` covers cancel/conflict, checkpoint failure, replaced targets and faults before/after index publication. Host `primary_location_idle_notice_prefix_and_missing_cwd_repair` proves zero inference and the actual tool cwd. |
| R13 busy ordering and original task cwd | Native core fixture runs an entire real tool batch across a pending move. Same-batch and already-running background tools keep their original cwd; the subsequent tool uses the new binding. Lost index/ack reconciliation never adds a second notice. |
| R14 preserved historical source and instructions | Native core fixture compares prior messages, system text and active profile, repairs a missing cwd, explicitly replaces the profile, splits and clears. Existing startup/instruction/Session/context suites preserve exact capture and projection owners. |
| R14 prompt-safe failure integration | `durable_primary_preflight_preserves_identity_and_reconciles_rollback`, `durable_injected_preflight_retains_prior_tool_history_and_committed_input`, the real blocked-startup stream-pair journey and `durable_context_action_restores_only_the_exact_primary_input`. UUID correlation supplements the existing request/content checks. No context projection or curator policy is replaced. |
| R14 actual human recovery | The native TUI fixture returns HTTP 413 from its localhost provider and requires exact text/image restoration, a failed rolled-back input receipt, no automatic resend and preserved earlier effects. `verify_primary_tui.py` exercises busy navigation, independent Clear, retained-source return and narrow reconnect. |

Counts alone do not establish these rows. Preserve each runner result, exact
binary identity, fixture cleanup and requirement-specific assertions.

## Native reproduction

Use an explicit immutable candidate or activated binary. Each script requires the
existing isolation wrapper and creates only owned roots, sockets and processes:

```sh
python3 scripts/run_isolated_test.py python3 scripts/verify_primary_tui_input.py \
  --binary /absolute/immutable/jcode --artifact-dir /private/tui-evidence
python3 scripts/run_isolated_test.py python3 scripts/verify_primary_input_location.py \
  --binary /absolute/immutable/jcode --artifact-dir /private/core-evidence
python3 scripts/run_isolated_test.py python3 scripts/verify_primary_tui.py \
  --binary /absolute/immutable/jcode --artifact-dir /private/navigation-evidence
```

Run latency-sensitive native fixtures separately from expensive compiler and Git
suites. The TUI proxy suppresses fixture replies and closes fixture connections,
not real user work. Escape is Stop while a primary is active, so a reattached busy
fixture must not blindly dismiss presumed onboarding with Escape.

Tester termination is not a saved-input reload operation. These fixtures do not
claim autosaving every unsent keystroke through SIGKILL/SIGTERM. Exact explicitly
saved image/retry restoration and ambiguous legacy snapshot retention are tested
through the existing production snapshot owner. Accepted durable input has the
stronger independently verified process-replacement contract.

## Final gates and limits

Use coordinated selfdev tests/builds and final build-reload. Check strict lint,
workspace formatting, full-range whitespace, protected paths, actual binary,
current/shared channels and canary after reload-sensitive tests. A candidate
build is not activation. Implementation acceptance and downstream publication
are separate later gates, recorded in the private program handoff.

Native evidence is macOS arm64 with local scripted provider traffic. It does not
establish hosted model quality, vendor billing, vendor compute cancellation,
Linux/Windows service parity, shell sandboxing or arbitrary external-effect
rollback. No prompt wording grade or automated behavioral benchmark is used.
Memory/Swarm remain disabled. Managed location changes stayed staged at WP-04
acceptance, while inspection/cancellation of existing controls remained
available; later C01 packages delivered grants, checkouts, the service and the
rollout (see the [C01 integration ledger](C01_INTEGRATION_ACCEPTANCE.md)). C04
visuals and C05 final guidance keep their owners.
