# Tool execution and retained output

This downstream uses one shared execution owner for tool identity, retained output,
presentation, cancellation, and recovery. The implementation and verification
boundaries are detailed in [the developer guide](dev/EXECUTION_STORAGE.md) and
[the acceptance ledger](dev/EXECUTION_ACCEPTANCE.md). Mirza accepted WP-02 on
2026-09-13 at 14:30:30 UTC with the documented verification and platform boundaries.

## Retrieve output, do not repeat its effects

Non-read results are retained before presentation or context-pressure withholding.
A result identifies its durable `run-…` invocation and retained file. Errors and
cancellation preserve available output, original input, and completed effects.
Read the retained file when more detail is needed. Do not repeat a build, mutation,
email, or other operation merely to retrieve its output.

`accept_large_output=true` no longer authorizes reexecution. Legacy false/absent
values are inert compatibility input. Tools instead expose optional `output_size`:

| Alias | Nominal Unicode-character target |
|---|---:|
| `very_small` | 10,000 |
| `small` | 20,000 |
| `medium` | 40,000 |
| `large` | 60,000 |
| `very_large` | 100,000 |

A positive integer is also valid. A per-call choice wins over the per-tool setting,
then the general setting, then defaults: 40,000 for read and 20,000 otherwise.
Content that fits is complete. Otherwise the shared policy prefers the nearest
line boundary in 70–130% of the target, choosing the shorter prefix on a tie; EOF
is a boundary. Without a suitable boundary it cuts safely within the line and
provides exact continuation. These are character targets, not exact token limits.

Context pressure can still withhold delivery. The non-read result remains saved.
Use the existing Context Editor/`/compact` workflow to free context, then retrieve
that result. Withheld pages do not advance the delivered source position.

For external tools whose argument names conflict with framework controls, follow
the advertised wrapped `arguments` schema. A producer-owned argument is not silently
stripped or interpreted as an output-control request.

## Exact reads and media

Text read accepts ordinary line ranges and a `read_point` returned by an earlier
page. A point replaces start/offset/limit and can be combined with `end_line`.
Use the same logical source. A changed ordinary file or changed sealed part rejects
its old point. Managed live output exposes only committed, verified bytes; unchanged
output and part points survive verified archive relocation. Plain source reads do
not create an archive copy of the whole source file.

PDF extraction retains complete selected derived text, not a duplicate original
PDF. Continue through the derived-text path rather than repeating extraction.
Images are atomic, subject to the existing 20-MiB acquisition bound and provider
support. Canonical retained image binary references are read through their manifest
and integrity evidence instead of being treated as anonymous `.bin` files.

## Waiting and Stop

Ordinary calls remain foreground unless explicitly backgrounded or promoted.
An explicit foreground deadline can promote the same execution, not launch another
copy. A background acceptance receipt is not a terminal result. Existing `bg`
operations accept durable run IDs for status, wait, output, tail, and cancellation.

Cancelling foreground parent work stops the foreground work it owns. Cancelling a
wait on explicitly backgrounded work stops that wait, not the command. Explicit
Stop targets the selected owned execution. Native process groups receive TERM and,
when needed, KILL; the owner keeps waiting for actual quiescence. Uninterruptible
work is not falsely reported as stopped. User applications intentionally launched
by `open`/`reveal` are completed handoffs, not disposable tool processes to kill.

Cancellation does not roll back files, unsend mail, compensate remote services, or
kill shared MCP/browser/provider services. Remote computation acknowledgement is
not promised. Reload quiescence is distinct from human cancellation. Supported
native background commands preserve identity across daemon replacement; uncertain
owner loss produces an inspectable interruption rather than automatic replay.

A worker's final control connection can close after it seals output. Wait uses the
durable terminal receipt in that race, including failed/cancelled outcomes, rather
than mistaking control-handler shutdown for cancellation of the completed command.
Managed background output and previews use the same archive identity and integrity
checks as ordinary retained-output reads.

## Storage and remote clients

Execution metadata and receipts stay in private local state. Configured emergency
output placement can use a UUID-verified archive volume when local receipt headroom
would otherwise be exhausted. Successful placement is quiet. Offline requested
output fails explicitly; it is not reconstructed by repeating the producer. Mirza
accepted Active's existing unencrypted, ownership-ignored protection boundary.
Routine seven-day archival, reader-owned snapshot pruning, immutable transcript
inspection and trusted-client cleanup are described in [session inspection and
retention](SESSION_INSPECTION.md). The native [`/tasks` monitor](TASK_MONITOR.md)
provides these controls and reviewed cleanup without a separate execution owner.

Harness/SDK clients use `shared_execution_v1` for list/inspect/read/Stop/background
and `shared_execution_parts_v1` for bounded binary-part materialization. Paths in
responses are server-owned, not assumed mounted on the client. Binary continuation
is digest-bound. See [SDK usage](../sdk/typescript/README.md#retained-executions).

macOS arm64 is the native verification target. Portable DTO/transport checks are
not proof of complete Windows/Linux application support. Unsupported storage or
process-control capabilities fail explicitly; no weaker ownership fallback is
claimed as equivalent support.

## Native task monitor

[`/tasks`](TASK_MONITOR.md) provides metadata-only lists, bounded live text previews,
original-owner Stop/background/conditional force controls, and reviewed cleanup.
The Rust and TypeScript SDKs require `execution_force_stop_v1` before sending the
additional `force_stop` request. Existing execution and part paging remain unchanged.
