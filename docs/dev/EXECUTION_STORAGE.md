# Execution storage internals

This describes the shared storage/read owner in `jcode-base::execution`. The
Phase 4 WP-02 rollout is still being integrated. The library is not evidence
that every Registry, SDK, provider, or subprocess path already uses it.

## Ownership

The higher execution coordinator owns one complete invocation transaction.
Models, clients and SDK callers must not coordinate prepare/start/capture/seal
stages themselves. `Session` remains the authoritative conversation. Execution
metadata is a receipt/index, not a replacement transcript.

`ExecutionStore` keeps local SQLite metadata with WAL, full synchronization,
foreign keys, private files, short transactions and indexed metadata pages.
Invocation identity includes session, assistant-message identity and the entire
nested call path. An existing scope with different input is a conflict. An
existing invocation is not permission to repeat its producer.

`Capture` owns the streaming sink and the lease on its bundle. `ToolContext`
carries the lower-layer `OutputCapture` interface, avoiding an upward dependency
from tool-core. Inline output, an already captured result and a source-read page
are structurally distinct. Streaming producers return their own reference rather
than duplicating their body through the inline adapter.

## Retention and publication

A bundle contains canonical readable UTF-8, separate original text/stdout/stderr
bytes, observed drain-order events, received media parts and a manifest. Split
UTF-8 scalars remain buffered until complete. Invalid original bytes remain in
raw parts, with replacement-character rendering identified in stream metadata.
Observed drain order is not a claim about producer-time ordering.

Writes are bounded and apply backpressure. Successful retention requires data,
new directory entries and the committed-prefix metadata to be persisted. A
failed writer preserves its existing files. JSON serialization failure must not
cause a buffered writer's destructor to retry an already-retained prefix.

Sealing freezes one result/outcome. A failed terminal metadata write can retry
publication without capturing the body again or accepting a different result.
The durable manifest records terminal outcome and the canonical-text digest.
`recover_terminal_output` validates a sealed result under the output lease. It
can recover publication after owner loss, but cannot invent success from a
missing/legacy manifest or repeat an operation whose effects are uncertain.

Capture failure overrides a successful producer exit. A local failure receipt
identifies the preserved prefix/bundle even when archive storage has failed.
Failed terminal-receipt persistence remains an error, not reported success.

## Location and recovery

Metadata and operation journals remain local. The public output alias is stable;
physical data can be local or on a configured verified archive. The configuration
lives under `output.storage`, with local/archive reserve bytes and an optional
archive mount, volume UUID and relative dedicated directory. Reserves default
to 1 GiB each. No archive is silently selected by the library default.

The configured mount must already exist and its identity must match. Native
macOS verification uses the volume UUID, not just `/Volumes/Active`. Verified
directory handles pin filesystem identity. Unix creation, append, publication
and removal use validated leaves relative to owned directory descriptors. A
replaced or offline directory cannot redirect a writer into a local stand-in.

Initial allocation is journaled before its filesystem effects. It publishes the
identity file, empty canonical file, location metadata and alias before allowing
producer use. Recovery completes only the exact recorded allocation.

A relocation records its exact source/destination, directory identities and
part sizes/digests. It copies and verifies before publishing the new location
and alias. Only then does it delete verified original parts. Recovery rejects
contradictory locations, symlink parts and conflicting destination contents. A
partial destination can resume only when its existing bytes are an exact source
prefix. Duplicate recovery does not increment the location generation again.

`recover_output_storage` handles the existing allocation and relocation
journals. It does not scan for orphan-looking files to delete. Week-old archival,
activity clocks, snapshot pruning and human cleanup selection are separate
consumers of this owner, not a second mover.

Active's existing unencrypted, ownership-ignored configuration is an explicitly
accepted downstream boundary. Normal local permissions and safe-export policy
still apply, but Unix mode bits on that volume do not provide stronger isolation
than the mounted volume actually enforces. Storage code does not change volume
ownership or encryption settings.

## Source reads

`SourceReader` streams a bounded UTF-8 presentation window and stores small
versioned continuation points, not a duplicate full ordinary source file. The
common character selector counts generated line labels as well as source text.
It uses Unicode scalar counts and the 70–130% nearest-line-boundary policy, with
a shorter-prefix tie break. Tiny targets fail rather than produce zero-progress
cursor loops. A changed ordinary source rejects its old point. A retry point
identifies the beginning of a page that was withheld rather than delivered.

Managed live-output/relocation points and derived PDF/media integration remain
WP-02 integration work. Do not claim them from ordinary-source reader tests.

## Verification

Run through coordinated self-development tests:

```sh
scripts/dev_cargo.sh test --profile selfdev -p jcode-base --lib execution:: -- --test-threads=1
scripts/dev_cargo.sh clippy --profile selfdev -p jcode-base --lib --tests -- -D warnings
```

The tests use real SQLite/filesystem owners with controlled capacity, write and
publication faults. They cover every exposed allocation/move interruption
boundary, exact partial-write preservation, duplicate recovery, directory and
symlink replacement, unavailable storage, terminal retry/corruption and
independent-process invocation admission. These subprocesses are ordinary test
executables, not model agents.

The ignored native archive fixture requires an explicit
`JCODE_EXECUTION_ARCHIVE_FIXTURE` JSON file describing a new isolated directory
whose name starts with `jcode-execution-fixture-`. Invoke its exact test with
`--ignored`. It verifies the real volume, initial emergency placement, writer
relocation and stable alias reads, then removes only its own fixture. It forces
the reserve policy, not actual host disk exhaustion. Never point it at user
output data or treat a skipped native check as passing evidence.

Current mandatory native acceptance is macOS arm64. Other native platform
coverage and capability limitations must be recorded explicitly during rollout.
