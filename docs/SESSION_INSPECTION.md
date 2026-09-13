# Session inspection and output retention

## Inspect a conversation as it was

`session_outline(target)` opens a durable as-of snapshot. Targets are `self`,
`parent`, or a session readable through the caller's conversation relationships.
The outline includes user/assistant text, structural tool summaries and context
transformation markers. It returns a `snapshot-...` ID.

Use that exact ID for subsequent inspection:

```text
read_transcript(snapshot_id, range?, raw=false)
expand_tool_use(snapshot_id, tool_use_id)
```

- Ranges are inclusive, one-based **source message positions** shown in the outline.
- The default transcript honors the captured context projection. An intersecting
  summary is returned whole, with its actual coverage. `raw=true` reads original
  stored messages without undoing or changing context.
- Tool expansion uses the snapshot-bound reference shown by the outline, not an
  ambiguous provider tool ID. It includes original transcript arguments, verified
  recorded execution input, and available retained output at the captured prefix.
- A running tool's captured status and prefix do not become its later live result.
  Open another outline to capture the latest state.
- Complete results use ordinary retained-output presentation and `output_size`.
  Read the saved result or continuation rather than repeating the original work.
  Inspection output streams to capture in bounded chunks and preserves its available
  prefix on Stop. Its metadata/manifest retains snapshot correlation.

Snapshots survive source edits, rewind, resume and process reload. They do not
activate the inspected profile or skill in the reader. Primary continuation
sessions may read inherited ancestor-owned references without gaining control or
changing snapshot ownership. Isolated-child creation and its additional relationship
integration are a separate feature. No child launch or task-monitor UI is implied
by these inspection tools.

## Snapshot lifetime

After reader A has been idle for seven days, automatic pruning keeps its newest
**two snapshots per distinct target B**. Ten targets with four snapshots each leave
20 retained snapshots. Live execution excludes a session from idle maintenance.
In-flight snapshot reads are protected until they finish.

Pruned IDs fail explicitly. Pruning does not expire sessions or remove inspection
results already delivered into a conversation. Shared immutable blobs are reclaimed
only when no retained snapshot references them. Reclamation failures are reported
separately from the count of snapshots whose pruning already committed.

## What counts as activity

User/parent messages, explicit opening/resumption and actual model/tool work count.
Listing, status polling, automatic UI refresh, maintenance scans and metadata-only
Session saves do not. Agent A inspecting B counts for A, not as resumption of B.
Explicit trusted-client Session inspection also counts as use of the inspected
session. The clock is durable execution metadata, not filesystem access time.

On migration, legacy activity evidence is retained without inventing a newer
user-activity event. A one-time observation floor prevents retention before a full
week has elapsed under the new tracker. A lost model-turn owner similarly receives
a fixed first-observed-stop bound, not a clock refreshed by every scan.

## Output storage on Active

Local metadata and snapshots remain below Jcode's private state root. Non-read
output bundles have stable aliases under `execution/outputs/<run-id>/`. After the
owning session's idle week, sealed bundles move to the configured archive through
a copy/verify/publish/delete transaction. Resume reads archived output **in place**.
It does not move it back.

The existing `[output.storage]` configuration owns archive mount, volume UUID,
dedicated directory and local/archive headroom reserves. Mirza's downstream archive
is Active, UUID `5B3BF7CE-D42A-432A-80C6-A7279A89DC77`, with directory
`jcode/tool-outputs`. The mount label alone is not identity. An absent or wrong
volume never becomes a local substitute directory.

Active's existing unencrypted, ownership-ignored protection level is explicitly
accepted. Local permissions and existing export policy still apply, but mode bits
on Active are not a stronger privacy guarantee. Jcode does not change volume
ownership or encryption.

Routine unavailable archival is quietly deferred. A requested offline result
returns targeted unavailability, not an instruction to rerun its producer.
Emergency reserve-aware capture remains independent of cold archival. If usable
storage cannot retain output, capture fails honestly with its available prefix.

## Reviewed cleanup through trusted clients

The backend is available before the convenient task-monitor menu. It is not a
model-advertised deletion tool. Harness API v1.4 and both SDKs advertise:

- `session_inspection_v1`
- `output_cleanup_review_v1`

For an already attached TypeScript client:

```ts
const snapshot = await client.sessionInspection(sessionId, {
  action: "outline", target: "self", output_size: "small",
});
const transcript = await client.sessionInspection(sessionId, {
  action: "transcript", snapshot_id: snapshot.snapshot_id, raw: true,
});
const status = await client.outputCleanup(sessionId, {action: "status"});
const preview = await client.outputCleanup(sessionId, {
  action: "review", selection: {selection: "oldest_bytes", bytes: 1_000_000_000},
});
// Present the exact preview to the user. Only after that single confirmation:
if (preview.kind === "review") {
  const outcome = await client.outputCleanup(sessionId, {
    action: "confirm",
    review_id: preview.review.review_id,
    confirmation_id: preview.review.confirmation_id,
  });
}
```

Rust equivalents are `session_inspection` and `output_cleanup`, using the exported
request/response types. SDKs reject unsupported capabilities before transport.
Responses stay correlated to the original attached session across later attachment
changes. Server-owned paths are retrieved through the existing execution read/part
API rather than assumed to exist on a remote client.

Cleanup defaults to completed cold-archived **whole outputs**, excluding live work
and recent emergency spillover. Preview reports exact candidates, bytes, any
whole-output overshoot and affected snapshot IDs. Explicit selection uses
`{selection: "outputs", run_ids: [...]}`. A stale or wrong confirmation performs no
new deletion. The accepted plan needs one confirmation, not one per file.

This is the existing **trusted same-user client boundary**, not proof of a physical
human origin and not an OS sandbox against an agent-launched script. No additional
human-attestation mechanism is imposed.

Deletion preserves source conversations, already delivered results, original
invocation inputs, project files and child-authored artifacts. It may deliberately
remove backing output referenced by snapshots, as disclosed in the preview.
Tombstones distinguish that deletion from an empty result or an offline volume.
Per-output failure may include partial filesystem effects; `deleted=false` means
completion was not established, not that nothing changed. Retry/restart resumes
only confirmed operations, never merely reviewed plans.

## Recovery and implementation

The quiet worker uses the originating store namespace and runs maintenance on a
60-second cadence without waking a model. A kernel lease serializes maintenance
across runtimes. Status reports are metadata, not output-body reads. New inspection
requests run independently of the inspected Agent mutex, and a blocked inspection
does not prevent Stop or metadata requests on the same client connection.

Strict Session capture rejects corrupt/torn journals, unsupported active writers
and inconsistent source identities rather than silently migrating or salvaging
history. Ordinary Session restoration remains its separate recovery entry point.
Do not repair an unavailable output by repeating an operation with uncertain effects.

See [implementation and persistence details](dev/SESSION_INSPECTION.md) and
[shared output/control storage](dev/EXECUTION_STORAGE.md). Native verification is
macOS arm64. Portable DTO/SDK compilation does not establish native Linux/Windows
storage or process-control parity.
