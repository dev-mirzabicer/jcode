# WP-02 requirement-to-evidence reconciliation

Status: **accepted by Mirza on 2026-09-13 at 14:30:30 UTC**, including the documented evidence and platform boundaries. The exact approved candidate `6ad23e42b` was published to local and downstream main. Acceptance does not convert failed aggregate suites into passing results. `EXECUTION_STORAGE.md` describes implementation, `EXECUTION_PRODUCERS.md` classifies producers, and `EXECUTION_NATIVE_ACCEPTANCE.md` records actual binary workflows. The accepted program work-package report controls the durable handoff.

## Primary work-package requirements

| Requirement | Implementation / concrete evidence already observed | Final reconciliation |
|---|---|---|
| A14 complete non-read capture | Registry owns input, execution and output before presentation. Native producer, direct Bash, batch, SDK/CLI ingress, MCP, HTTP, computer/browser helpers, mutation, selfdev and Gmail acquisition fixtures preserve sentinel tails/typed parts. | Validated across the reconciled producer inventory; native standalone/shared and post-activation command/retrieval paths verified. Adapter-specific evidence is retained, not inferred solely from a generic fixture. |
| A15 raw streams, rich output and errors | Separate stdout/stderr and observed event streams, incremental UTF-8 handling, retained binary parts, original SDK records, MIME/resource integrity, malformed CLI/MCP/HTTP cases and partial-mutation/capture failures are tested. Native CLI and Harness retrieve full raw output. | Validated by raw/typed/partial-failure suites and native binary-page reconstruction. Explicit part retrieval does not silently embed the archive in ordinary exports. Hosted media interpretation is not claimed. |
| A16 scoped nested identity | Invocation IDs bind session, authoritative message and full call path. Repeated batches/equal raw IDs, preserved member metadata/media, replay conflicts and transcript-order repair tests pass. | Validated by scoped replay, batch and history families, including eight runtime and 33 integrated execution checks after fixes. Batch-in-batch remains intentionally prohibited. |
| A17 retrieval without reexecution | Legacy framework true rejects before producer effects. Shared guard retains output, exposes references, resets withheld read points, and tests count one producer through replay/retrieval. Actual JSON/NDJSON/Harness fixtures verify one append-counted command and later tail retrieval. | Verified after activation: native output was withheld, an explicit existing reasoning-suppression transaction was applied, and retained-tail retrieval succeeded with one original effect. The guard was the single-output bound; no claim of a full live conversation or economic necessity. |
| A18 shared character selection | Common scalar-count selector and typed aliases/raw values. Tests cover 70–130% boundary selection, shorter ties, EOF, CRLF, Unicode, huge lines, labels, tiny targets and exact reassembly. Config aliases normalize and conflicts reject. | Validated by scalar selector/read/configuration suites; native retained reads pass after activation. Explicit aliases and conflict rejection are production-parser tested. No prompt-prose quality claim. |
| A19 exact source continuation | Strict ordinary-file versions, bounded reads, deterministic points, append-safe managed chunks, part digests, stale/corrupt/offline rejection and cancellation/no advancement. Real archive fixture preserves logical points without move-back. | Validated by ordinary/managed/legacy read and corruption/cancellation suites; real archive relocation and post-activation native reads verified. Plain reads do not archive a full source copy. |
| A20 PDF and atomic media | Complete PDF-page extraction into retained derived text, explicit ranges and continuation without the original PDF. Atomic image bytes, bound checks, verified retained image reads, raw resource paging and real archive relocation are tested. | Validated by complete derived-PDF, atomic image and verified retained-media/resource tests plus real archive continuation. Existing acquisition/provider support bounds are retained; hosted vision quality is untested. |
| A25 emergency storage | Private local index, receipt reserve, UUID-bound Active placement, backpressure, partial ENOSPC/EIO, exact-prefix relocation, transaction checkpoints and real native archive fixtures. | Validated by injected I/O/ENOSPC and publication recovery plus the real verified-volume fixture. Live output configuration is installed, Rust-validated and byte-verified after activation. Routine age archival remains WP-03. |
| A27 real Stop ownership | Red/green adopted-inner-task and human-versus-reload defects; native process trees, quiet descendants, denied signals, helper launch provenance, cooperative mutation boundaries and selfdev watcher isolation. Actual daemon individual/parent Stop, cancelled background wait, and 80/60-column native Escape pass. | Verified native individual/parent/Escape Stop, background-wait isolation and daemon restart after activation. Owner/helper and denied-signal tests preserve actual quiescence. Acknowledgement is not terminal completion. |
| A28 provider/MCP/helper cancellation | Quiet HTTP, persistent WebSocket chain ownership, Cursor H2 subtasks, CLI admission/stdin/process groups, MCP pending cleanup/late replies, computer/search/browser helper raw-prefix cases. | Validated through real local HTTP/WS/H2/MCP and owned subprocess fixtures. D-28 limits only optional ChatGPT-web hardening. No vendor-specific compensation or remote-compute acknowledgement is claimed. |
| A29 reload/crash without duplicate effects | Gated native workers, boot/birth identity, process-image/lease proof, helper launch gaps, terminal-witness recovery, immutable unknown-outcome receipts, scoped Session repair and namespace-bound SDK acquisition acknowledgement. Real daemon/bridge restart preserves background identity and one effect. | Validated in storage/worker/history recovery suites and verified native daemon replacement, coordinated shared reload, passed canary and post-activation workflows. Legacy unproven owners/bytes fail explicitly; no PID guessing. |
| A30 metadata integrity | SQLite-owned descriptors with NOFOLLOW, separate initialization lease, current-schema read-only opening, WAL/rollback lock regressions, true multiple-process admission, publication failures, exact terminal retries, corruption/permissions and Session receipt projections. | Validated by full store families, real multi-process admission/locking, failure/retry/corruption/private-mode tests and authoritative Session acknowledgement projections. Historical red findings and failed aggregates stay preserved in the failure inventory. |
| A33 argument/capability compatibility | Frozen schema/input/producer binding, conservative external wrapping, collision tests, legacy false/absent behavior. Harness v1.3 execution/part capabilities reject unsupported servers before transport. Reply correlation survives attachment changes. | Validated by complete Harness/Rust SDK suites, 49 TypeScript tests and native daemon/SDK operations. Portable contracts/transport compile for Linux/Windows; full cross builds are blocked by native C toolchains, and non-Unix ownership support is not claimed. |

## Cross-cutting disposition and review boundaries

- A01/A38: retain hard-disabled Swarm/memory, startup context, frozen profiles/skills/roster and protected context behavior. Focused prerequisite families pass; the complete per-test failure inventory distinguishes repaired active paths from disabled-feature expectations.
- A31/A32: metadata-only listing/control with bodies unavailable or large, standalone JSON/NDJSON, shared native TUI/Harness and SDK callers. Native Run is local, not daemon-backed by `--socket`; documentation is corrected.
- A37: owner-only local artifacts, accepted Active protection level, scoped runtime credentials, redacted exports/logs and no implicit full-archive inclusion. Byte-part reads authorize the same-user connection and never accept arbitrary client filesystem paths.
- A39: actual configuration migration, current tool/help/recovery docs, full staged/commit review, source/runtime identities, coordinated shared activation and passed canary. Shared/current/running 3485be42d-dirty-39bb6a43a849 and passed canary are verified. Twelve post-activation native cases pass. Final phase capability-map updates are not prematurely claimed in this WP.
- Platform support: macOS arm64 is mandatory native evidence. Unsupported storage/process/force-stop operations must fail visibly. Full Windows/Linux application or native-runtime parity has not been established by DTO checks.
- Legacy outputs: only existing bytes and recorded facts can be recovered. Legacy background read/status compatibility and original-digest index import tests preserve available data. Unverified active producers never acquire new ownership guarantees by assumption.

## Verification-harness incident and repair

See [per-test failure dispositions and follow-up ownership](EXECUTION_FAILURE_TRIAGE.md)
and its complete JSON inventory for all 135 failures in the two completed bounded
runs. Active-path findings were handled within WP-02. Default-disabled feature
expectations and nonurgent fixture-suite maintenance are explicitly distinguished
from production regressions and assigned to future maintenance rather than hidden.

The first complete base suite finished with 1,549 passed, 34 failed and 3 ignored.
The following app-core suite then hung on an enabled-behavior Swarm broadcast test
whose unbounded receive awaited an event suppressed by the global retirement gate.
After Mirza reported the overnight stall, the exact owned test process was stopped,
its logs preserved, and the independent strict-lint step completed successfully.
This was not a completed app-core suite and is not counted as one.

Three synthetic broadcast tests now explicitly isolate their feature/configuration
preconditions and have five-second deadlines. They exercise records/channels only,
not Swarm agents. All three passed with strict lint. A progress-aware external
watchdog is now used for broad verification: no-progress and total-suite deadlines
produce failure plus diagnostics, never a passing result. Its completion/stall/
heartbeat self-tests passed.

The base failures also exposed test-root leakage. The shared authentication fixture
now isolates the durable runtime registry as well as the home, and the remaining
handwritten prompt fixtures reuse that owner. Existing prompt assertions were not
changed. The complete 19-test prompt family passes with its explicit synthetic
Swarm-enabled precondition. Four session-context tests now compare OS-resolved cwd
identities and restore cwd on assertion unwinding, preventing a failing fixture from
leaving later tests in a deleted directory. Those four tests pass.

A simulated reload test had written a synthetic canary marker to the default home.
The exact contaminated manifests and synthetic reload context were preserved in
private evidence. Only the known canary/pending fields were restored to the verified
WP-01 state, and the exact synthetic reload context was moved out of the live
recovery namespace. Binary channels and unrelated manifest fields were unchanged.
Simulated reload now rejects an absent explicit `JCODE_HOME` before metadata access;
its regression and strict lint pass. Broad runners isolate both `HOME` and Jcode
state, while retaining explicit Cargo/rustup cache locations. No new candidate
canary success is inferred from restoration of the previously passed WP-01 marker.

Subsequent bounded full runs completed: base 1,567 passed / 16 failed / 3 ignored,
and app-core 1,398 passed / 119 failed / 25 ignored. These remain failed suite
results. Base's remaining non-feature failure was a home-only startup-staleness
fixture; its private runtime guard then passed the focused regression. The other
base failures require enabled memory/Swarm semantics that are not the live policy.

The Agent startup/profile family subsequently passed all 21 tests after fixture
isolation and explicit synthetic routing preconditions. The main Agent family
passed 84/86 before two remaining home-only fixtures were identified; both were
then fixed and passed independently. This does not rewrite the earlier full-run
counts or imply every unrelated dormant workflow was reconciled.

The reload-family investigation also produced two real red regressions: a closed
compatibility channel hid a sealed result, and delayed completion could resolve a
same-ID record from the wrong store. The waiter now resolves the authoritative
owner; managed keys and publication bind to their original store. Eight runtime,
19 background and 33 integrated execution tests passed, including both native
reload cases. Final strict base/app-core/TUI lint passed after correcting two
nested-condition test lints, with no suppression. Uncertain ownership still blocks
reload rather than being silently dropped.

Remaining default-disabled memory/Swarm expectation failures are not silently
converted into passes. They must be distinguished from active-feature regressions
in the final combined evidence. Original logs and failed attempts remain available.

## Lifecycle boundary

Mirza approved the candidate and its stated boundaries, and the exact candidate was fast-forwarded to local main and pushed to `dev-mirzabicer/main` on 2026-09-13 at 14:32:34 UTC. The subsequent accepted-status documentation tail changes no runtime implementation. Phase 4 remains incomplete; the accepted WP-02 progress report names WP-03 as the next authorized unit. No acceptance is inferred for later packages.
