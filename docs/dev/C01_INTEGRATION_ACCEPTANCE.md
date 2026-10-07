# C01 combined integration acceptance

SP-58-C01 (projects, checkouts and primary-session foundations) delivers
P01–P06 through twelve work packages. This ledger is the combined
verification route for its forty requirements (R01–R40) and eight journeys
(J01–J08) on one source tree. It records where each requirement's evidence
lives and the combined native run that exercises them together. It is not
Mirza's acceptance or a claim about the final C04 client or C05 guidance. The
C01 closeout's own findings are under "Phase closeout" below.

## Combined native matrix

Every C01 native journey runs on one immutable candidate binary, in sequence,
each through `scripts/run_isolated_test.py` with private state, owned daemons
and a localhost scripted provider. No real session, checkout, volume or the
user's login service is touched; fixture LaunchAgents are namespaced and
removed.

| Journey script | Requirements | Notes |
|---|---|---|
| `test_physical_locations.py` | R01, R02 | Legacy keys, capture, volume binding |
| `test_workspace_catalog.py` | R03–R06 | Organization, archive, backup/restore, process restart |
| `verify_primary_launch.py` | R07, R08 | All placements, Run/REPL/Harness/ACP, replay |
| `verify_primary_tui.py` | R07, R11, R14 | PTY launch, busy navigation, Clear, narrow reconnect |
| `verify_primary_stream.py` | R11 | Slow observer resnapshot |
| `verify_primary_review.py` | R08, R11, R09 | Detached notify, peers, managed Clear; legacy Clear refused until adoption |
| `verify_primary_host.py` | R10 | Ordinary daemon, more than 330 clientless seconds, Stop |
| `verify_primary_input_location.py` | R12–R14, R33 | Durable input, moves, split/clear, crash then selected continue |
| `verify_primary_tui_input.py` | R12 | Lossy proxy, same-UUID retry, images |
| `verify_native_write_scope.py` | R09, R15–R20 | Native destinations, grants, carry, SDK Clear |
| `test_workspace_checkouts.py`, `test_workspace_clone_cancel.py`, `test_workspace_git_auth.py` | R21–R24 | Local/remote clones, cancel, credential helpers, submodule/LFS trust |
| `test_workspace_closeout.py` | R25–R28 | Bridge and both SDKs; needs `JCODE_WP07_BRIDGE`, `JCODE_WP07_RUST_PROBE` |
| `test_workspace_closeout_retention.py` | R29 | Needs `JCODE_WP07_SESSION_PROBE` |
| `test_runtime_cli.py` (`--sdk-bridge`, `--sdk-rust-probe`), `test_runtime_work.py` (`JCODE_WP08_PHYSICAL_TUI=1 JCODE_WP08_NAMESPACES=1`) | R30, R31 | Reviewed shutdown, survival, namespaces |
| `test_runtime_supervision.py` | R32–R35 | Reload/restart continuation, crash recovery, isolated LaunchAgent, power |
| `verify_workspace_manager_tui.py` | R36, R37 | Physical-input `/workspace` and `/runtime` journey |
| `verify_workspace_contracts.py` | R38 | Harness/SDK contracts, agent `workspace` tool |
| `verify_session_placement.py` | R07, R09, R13, R40 | Rollout: refusal before acceptance, propose/place, `run --place`, `acp --place`, TUI placement review |
| `verify_c01_combined.py` | J01 | Two repositories, directory, work area, shared clone, project cwd, standalone, archive |

Every script places its daemon socket under the artifact directory, so keep
that path short: macOS limits Unix socket paths to 104 bytes and the daemon
exits with `path must be shorter than SUN_LEN` otherwise.

Probe binaries: `cargo build --profile selfdev -p jcode-harness-api-server
--bins` (bridge), and `cargo test --no-run -p jcode-sdk --test closeout_native
--test runtime_native` and `-p jcode-base --test closeout_retention_native`.

### Fixture repairs found by the combined run

The first combined run (candidate `e02817c93`) passed 18 of 22 scripts. The
four failures were fixtures written before later accepted packages, or test
environment, not product defects; each also failed or would fail on the
accepted `7eb60ad47` baseline:

- `verify_primary_review.py` expected Clear on a legacy session to succeed with
  managed launch on. Since WP-05 it is refused until adoption.
- `verify_primary_input_location.py` compared whole split location records
  (a split publishes its own revision and operation) and expected an
  unexpected exit to resume inference (WP-09 requires a trusted Continue). It
  now asserts the recovery item, the waiting input, no inference, then one
  selected continuation and the queued input.
- `test_workspace_closeout_retention.py` failed only because the artifact path
  was too long for its Unix socket.
- `test_runtime_supervision.py` counted the developer runtime's own command
  workers when checking the survivor's power assertion; it now counts only the
  fixture's workers.

## Requirement map

Labels follow the verification standard. "Package ledger" names where the
detailed sub-criteria and test names are recorded.

| ID | Package ledger | Combined route | State |
|---|---|---|---|
| R01–R02 | [PHYSICAL_LOCATIONS.md](PHYSICAL_LOCATIONS.md) | `test_physical_locations.py`; key preservation in the real rollout | Verified (macOS arm64) |
| R03–R06 | [WORKSPACE_CATALOG.md](WORKSPACE_CATALOG.md) | `test_workspace_catalog.py`, `verify_c01_combined.py`, management journey | Verified |
| R07–R08, R10–R11 | [PRIMARY_HOST_ACCEPTANCE.md](PRIMARY_HOST_ACCEPTANCE.md) | launch, TUI, stream, review, host journeys; placement journey | Verified |
| R12–R14 | [PRIMARY_INPUT_LOCATION_ACCEPTANCE.md](PRIMARY_INPUT_LOCATION_ACCEPTANCE.md) | input/location and TUI input journeys | Verified |
| R09, R15–R20 | [WORKSPACE_SCOPE_ACCEPTANCE.md](WORKSPACE_SCOPE_ACCEPTANCE.md), [NATIVE_WRITE_SCOPE.md](NATIVE_WRITE_SCOPE.md) | native write scope journey | Verified; shell and external effects are not contained |
| R21–R24 | [WORKSPACE_CHECKOUTS.md](WORKSPACE_CHECKOUTS.md) | three checkout journeys | Verified with local bare remotes and local LFS; hosted credentials not claimed |
| R25–R29 | [WORKSPACE_CLOSEOUT_ACCEPTANCE.md](WORKSPACE_CLOSEOUT_ACCEPTANCE.md) | closeout and retention journeys | Verified on disposable fixtures only |
| R30–R31 | [RUNTIME_SHUTDOWN_ACCEPTANCE.md](RUNTIME_SHUTDOWN_ACCEPTANCE.md) | runtime CLI/SDK and work journeys | Verified (supported native workers) |
| R32–R35 | [RUNTIME_SUPERVISION_ACCEPTANCE.md](RUNTIME_SUPERVISION_ACCEPTANCE.md) | supervision journey; real service activation recorded per package | Verified (macOS arm64) |
| R36–R37 | [WORKSPACE_MANAGEMENT_ACCEPTANCE.md](WORKSPACE_MANAGEMENT_ACCEPTANCE.md) | management TUI journey | Verified; typed Force validated |
| R38 | [WORKSPACE_CONTRACTS_ACCEPTANCE.md](WORKSPACE_CONTRACTS_ACCEPTANCE.md) | contracts journey | Verified |
| R39 | This ledger and the [rollout guide](../WORKSPACE_ROLLOUT.md) | Documentation reconciled; rollout rehearsed in fixtures, then on the real catalog | See "Rollout integration" |
| R40 | This ledger | Full matrix, placement and combined journeys, journey map | See "Journey map" |

## Journey map

| Journey | Combined route |
|---|---|
| J01 Organization and standalone | `verify_c01_combined.py` (two repositories, directory, flat area, shared clone, project cwd, standalone, archive with active work); management journey organization stages; catalog journey alias dedup |
| J02 Scope and trusted permission | `verify_native_write_scope.py` (cross-root denial, proposal, trusted grant, revocation, nested roots, child intersection); management journey approve/revoke; contracts journey forged-field proposal then SDK approval |
| J03 Volume and provisioning | Checkout journeys (local and bare-remote sources, submodule/LFS trust, cancel, credential helpers, interruption recovery); physical-location journey; management volume and clone stages; placement and launch journeys show no implicit clone |
| J04 Location and continuation | Input/location journey (busy move after the tool batch, idle move without inference, split/clear, profile replacement); placement journey; native scope carry yes/no; retention journey missing-cwd repair; management legacy adoption |
| J05 Detached work | Host journey past the old idle interval without debug; stream and TUI navigation journeys |
| J06 Runtime lifecycle | Runtime CLI/SDK and work journeys; supervision journey (reload/restart once, crash selection, isolated LaunchAgent, power); management runtime stages |
| J07 Closeout | Closeout journey (bridge and both SDKs, final and conditional authorization, interruptions); retention journey; squash-only history in the closeout fixtures (`squash-feature`); management closeout stages; contracts agent declaration |
| J08 Backup and consumers | Catalog journey backup/restore; management backup, export, refused and accepted import, restore; contracts C02/C04 consumer fixtures; SDK version refusals |

## Rollout integration (WP-12)

Turning on `features.managed_primary_launch` exposed two integration defects,
both repaired:

- With no catalog yet, every unplaced turn failed with a bare missing-file
  error. The control lease now reports `not initialized`, and an unplaced
  session skips location reconciliation.
- An ordinary new session was unplaced, so its first message was committed
  and then refused, with no placement route outside `/workspace`. Human input
  is now refused before acceptance, and the session is placed through a review
  that keeps its working directory ([managed placement rollout](../WORKSPACE_ROLLOUT.md)):
  - the TUI dialog on the first message, and `/place`;
  - `jcode run`/`repl --place`;
  - the `propose_placement`/`place` location commands with
    `session_placement_version = 1` (Harness API 1.14) in both SDKs.

The real rollout (below) found three more, also repaired:

- A tool that a Claude session receives inside a tool-set notice never had its
  managed guidance captured, so the description would have been re-rendered
  from the live store on each request. The capture now reads the session's
  tool-set record.
- A session placed through the first-send review lacked the initial location
  facts a launched session receives. They now join its one location notice
  message, and the notice calls the old placement `Unplaced`.
- Once placed, an agent could no longer write the agent scratch directory with
  native file tools, and read-only children never could. Every agent may now
  write it whatever its placement or permission
  ([agent scratch directory](NATIVE_WRITE_SCOPE.md#agent-scratch-directory));
  `native_scope_agent_scratch_is_writable_whatever_the_placement` and the child
  permission test cover placed, unplaced, mixed-patch, foreign-root and
  read-only-child cases.

Truthfulness repairs: `WorkspaceProbe` and catalog status report
`managed_rollout` from the flag, and `jcode runtime service status` reports
definition currency as unknown unless it compared a plan.

Runtime worker threads have an explicit 16 MiB stack
([server architecture](../SERVER_ARCHITECTURE.md#worker-thread-stack)). The
client attach path measured 1,692,304 bytes on the accepted WP-11 binary
against the 2 MiB default. With the explicit size, idle and five-session
resident memory of an isolated daemon were within 0.5 MB of the default, and
attach time was unchanged.

Placement evidence (`verify_session_placement.py`):

| Facet | Check |
|---|---|
| No catalog | Input refused with the placement message, not an I/O error; review reports not initialized; `run --place` refuses |
| Advertised | `session_placement_version = 1`, `location_enabled`, `managed_rollout` true |
| Refusal before acceptance | First message absent from history; no provider call |
| Propose and place | Default is the Git root, not the subdirectory cwd; stale revision is a conflict; place completes; replay returns the same record; the message then runs once |
| Reuse | A second session in the same directory is offered the registered location |
| Broad root | Home is offered with no default; `run --place` at home refuses |
| CLI | `run` without `--place` refuses before recording; with it, places and runs once |
| TUI | Enter opens the review over the conversation with the message held; Enter places and sends exactly once; Esc keeps the message and sends nothing; frames at 120×32, 80×24 and 48×12 |

Mechanism tests: base `workspace::session_placement` (7), TUI
`placement_review` (9), bridge `placement_review_crosses_the_bridge`, Rust SDK
`placement_review_requires_its_negotiated_version_and_exact_session`,
TypeScript `placement review requires its capability and correlates session
and request`.

## Real rollout

Mirza's runtime was moved to managed placement on 2026-10-05:

- **Catalog:** initialized, with backups `pre-organization` and
  `jcode-project` taken and verified (checksum and SQLite integrity).
- **Organization:** project `Jcode`, logical repository `jcode` (remote
  reference `dev-mirzabicer` only), the existing checkout
  `/Users/mirzabicer/src/jcode` and the directory reference `jcode_program`.
  Registration changed no files, branches or remotes.
- **Flag:** `features.managed_primary_launch` turned on; the config change was
  that one line.
- **Worker session:** adopted onto the checkout with its cwd moved from home.
  It received one location notice and one tool-set change adding `workspace`;
  its native write to `jcode_program` was refused, its access proposal was
  approved in `/workspace` and the write then succeeded.
- **New session:** a new TUI session in the checkout was placed through the
  first-send review and answered once.
- **Reload:** a real reload caused no tool-set churn.
- **Unchanged:** Startup Context and instruction-store state files, compared
  before and after each stage.
- **Tool latency:** discovery tool calls on the real catalog took 0.24–0.36 s
  end to end.

Unrelated performance and log findings from the rollout (first-session
preparation time, fresh-session history retries, completion retries for missing
sessions, a crashed empty session per self-dev build) predate WP-12 and are
recorded as program issues for later sessions.

## Phase closeout (SP-58-C01)

The closeout reran the combined matrix on the accepted source, traced the
rollout's interaction with every other session creator, and repaired what it
found.

**Matrix on the accepted source.** WP-12 ran the full 22 scripts on frozen
`94700139f` and only six of them on the later commits. The closeout ran all 22,
sequentially and on their first attempt, on a frozen copy of the activated
`9fd5881ee` binary (SHA-256 `47cff651…`). All 22 passed.

**Session creators under managed placement.** With the flag on, only a placed
session can run a turn. Three creators produced unplaced sessions:

| Creator | Before | Disposition |
|---|---|---|
| Scheduled `spawn` | The child copied the conversation but no placement; its turn was refused and the task failed with one log line | Repaired: the child takes its parent's placement and cwd through the Split scope owner, never carries direct grants, and an unplaced or missing parent fails before any child exists (`spawned_schedule_takes_parent_placement_without_direct_grants`, red without the repair) |
| ACP `session/new` | Unplaced; the refusal pointed to `/place`, which an editor cannot send | Repaired: `jcode acp --place` places new and loaded sessions at the proposed default (broad roots refused); without it the refusal names the ACP routes (`verify_session_placement.py` stage 6b) |
| `jcode debug create_session` | Unplaced; turns refused | Documented limit ([rollout](../WORKSPACE_ROLLOUT.md#other-session-creators)). Automatic placement would let a debug caller acquire any registered location's scope without review; recorded as program issue I-05 |

Ambient-mode cycles are also unplaced and refused; ambient mode is off by
default.

**Documentation.** Current-behavior guides that still called rollout, grant
carry, the Startup Context copy client or runtime controls staged were
corrected. Package acceptance records keep their statement as of their
acceptance and point here.

After the repairs, the full matrix and the focused families for the changed
owners passed again on the final candidate; the program's C01 completion report
records the exact runs.

## Limits

- Native acceptance is macOS arm64. Same-user trusted clients are not
  physical-human attestation. Native write scope is not a shell sandbox.
- Real-volume disappearance, power loss and hostile same-user writers are
  represented by fault injection and fixture volumes, not real events.
- Catalog backups hold metadata only, not checkout files or transcripts.
- Failures that reproduce on the accepted `7eb60ad47` test binaries: two
  disabled-Swarm routing tests in `tool::instruction_guidance` and `tool::bash`'s
  `bash_holds_a_risky_delete_until_justified_then_runs_it`.
- Two root `cli::tui_launch` tests fail under the isolated test runner: one
  compares `/tmp` with its `/private/tmp` alias and the other then hits the
  poisoned environment lock. WP-12 did not change that code. They were not rerun
  on the baseline; the INT-01 closeout recorded the same root result (226
  passed, 2 failed).
- C04 owns the final command-center visuals and old-picker retirement. C05
  owns the final closeout skill and agent guidance. C01's agent-facing text
  is the approved framework-stage `tools/workspace.md` and notice templates.
