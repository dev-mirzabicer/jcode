# Startup Context

This document describes Jcode's current Startup Context capability. It is both a user
workflow guide and the maintainer reference for project identity, capture, session
lifecycle, cache ordering, privacy, recovery, and module ownership.

## Core model

Startup Context lets the user maintain an ordered project default of files that Jcode
must provide to each new primary session before substantive work begins.

Jcode, not the model, reads the files. A successful capture becomes hidden user-authority
messages in the one authoritative transcript:

```text
Session.messages
```

The project default stores paths and external-target approvals. It does not store file
contents. The session receipt stores identity and delivery metadata. Exact captured text
lives once in the authoritative messages referenced by that receipt.

This design provides four distinct sources of truth:

1. **Saved project default:** the ordered path list used by future primary sessions.
2. **Current-session receipt:** what this session captured and its delivery state.
3. **Unsaved editor draft:** the user's local proposed selection and order.
4. **Durable apply status:** the server-owned state of a queued or recoverable operation.

The TUI keeps these sources visibly separate. It never treats an unsaved draft or path
selection as proof that a complete read occurred.

Startup Context is not agent memory. It remains available when agent memory is globally disabled
and does not infer, duplicate, or reactivate memory behavior. See
[`MEMORY_POLICY.md`](MEMORY_POLICY.md).

## Quick workflow

The production TUI shows a compact `Startup context` row above the composer. It reports
server-authoritative state without loading raw file bodies into ordinary chat.

Use:

```text
/startup
```

to open the full editor. The editor supports:

- Project browsing and bounded filename search.
- One-level directory bulk selection over direct non-directory children.
- Ordered selection, reordering, and removal.
- One exact external absolute path with resolved-target confirmation.
- Bounded current-file preview and metadata.
- Lazy exact inspection of captured receipt content.
- Keyboard and mouse interaction in wide, narrow, and extreme-narrow layouts.

The two primary actions are adjacent and equally prominent:

- `u`: **Use in this session**
- `p`: **Use in this session + save as project default**

Every apply begins with a fresh authoritative server preview. The review explains which
contents will be sent to the active model route, what will be persisted in session
history, and whether the operation can rebuild current-session startup messages or only
affect future state.

The main editor works with one responsive state machine. `Esc` unwinds the current layer,
such as external confirmation, apply review, result, search, receipt detail, preview, or a
nested directory. `q` closes the editor directly. The unsaved draft survives recoverable
failures and same-session reconnects.

## Project identity

Startup Context defaults are private server state keyed by the active project.

### Git projects

For a Git project, Jcode resolves:

- The canonical Git common directory as the project-plan identity.
- The canonical active worktree root as the path-resolution root for this session.

Consequences:

- Launching Jcode from different subdirectories of one repository uses one plan.
- Linked worktrees share the same ordered relative-path plan.
- Each worktree captures the current contents from its own active root.

Moving or copying a repository to another common-directory path creates a new local
project identity. Jcode does not guess identity from repository names or remotes.

### Non-Git projects

A non-Git project uses the canonical launch directory for both project identity and path
resolution.

If Jcode cannot establish a truthful project identity, primary startup blocks or the
non-interactive caller returns a typed failure. It does not fabricate an empty project.

## Selection, supported files, and bounds

The user controls every selected path and its order. Jcode does not suggest or add files
automatically.

A valid required startup file is a readable regular UTF-8 text file. Jcode rejects:

- Missing files and broken links.
- Directories and devices.
- PDFs, images, binary content, and non-UTF-8 content.
- Files that change repeatedly while being captured.
- Traversal and unsupported path representations.

Current complete-source safety bounds are:

- At most 1,024 selected entries in one project plan.
- At most 32 MiB of raw UTF-8 source in one atomic initial or late capture batch.

These are rejection bounds, not truncation limits. Jcode never truncates, summarizes,
omits, or reorders a selected file while claiming a complete read. The provider's real
safe request budget remains the final dispatch authority and may be tighter.

### Directory bulk selection

Selecting a directory is a one-time UI convenience. Jcode lists exactly one level, uses
deterministic natural filename order, ignores child directories, and submits every direct
non-directory child through the same validation path as an individually selected file.

The plan stores concrete files. It does not store a recursive rule, glob, or dynamic
directory membership. Files created later remain unselected.

### External paths and symlinks

An exact absolute path outside the active project is supported only after visible
confirmation. Approval is bound to the server-resolved target, not merely the entered
path. A project-relative symlink that resolves outside the project is classified as
external. If the resolved target later changes, Jcode requires confirmation again.

Selecting a file means its complete content will be persisted in the session and sent to
the active model route when included in a request.

## Capture and message order

For a non-empty valid initial plan, Jcode creates provider history in this order:

1. One hidden Startup Context control message.
2. One hidden exact file message per selected file in saved order.
3. The existing dynamic session-context message with date, runtime, Git, and working
   directory facts.
4. The first real user prompt later.

Each file message contains two separate text blocks:

1. Jcode-generated path, resolved target, ordinal, SHA-256, byte count, encoding, and
   completeness metadata.
2. The exact raw UTF-8 file text.

The metadata and raw content are separate so file prose cannot forge Jcode's generated
assertions.

An empty plan persists an empty receipt for UI truth but injects no Startup Context
control, file, reminder, or other agent-visible text.

## Captured, dispatched, and accepted

The TUI distinguishes three primary lifecycle facts:

- **Captured:** complete source and receipt were persisted. No provider invocation is
  implied.
- **Dispatched:** existing request preflight passed, dispatch metadata was persisted, and
  provider invocation began.
- **Provider accepted:** a provider stream event proved that the request was accepted.

The first provider dispatch freezes the initial captured bundle. A provider rejection
after dispatch does not make that historical snapshot editable again.

If provider-acceptance metadata cannot be persisted after acceptance, authoritative
history remains intact and the receipt enters visible metadata-repair state. Jcode does
not falsely downgrade accepted delivery.

## Initial blocking and prompt preservation

A fresh user-created primary context cannot dispatch while an initial required file is
missing, unreadable, unsupported, unstable, unapproved, or otherwise uncaptured.

The interactive TUI keeps the blocked session available for repair. A blocked send uses a
prompt-safe transaction:

1. Jcode attempts to durably remove the unanswered turn from authoritative history.
2. Only a successful rollback authorizes exact composer restoration.
3. The TUI restores raw text, paste backing values, images, cursor position, and input
   selection for the matching session and request.
4. Jcode does not resend automatically.

If rollback persistence fails, the action reports that the authoritative turn was
retained. The client does not duplicate it.

The same existing context-pressure mechanism remains authoritative when the complete
startup messages plus system prompt, tools, memory, attachments, and pending input exceed
the provider's safe request budget. Repair the selection or choose a model with sufficient
context, then submit manually.

## Applying changes

### Before first dispatch

A successful current-session apply can atomically rebuild the not-yet-sent Startup Context
bundle, including replacing it with an empty selection.

### After first dispatch

Established history is append-only for Startup Context:

- Newly selected files can become one exact late batch at the end of history.
- Removed files are not erased from established session history.
- Reordering cannot rewrite prior messages.
- Removal and reordering can still affect the saved default for future sessions.

A late batch contains one hidden update control message followed by exact file messages.
If capture fails, the entire late addition is rejected and the established session remains
unchanged.

### Busy turns and durable apply

If the agent is working, Jcode persists path-only queued intent and does not interrupt the
active provider request. At the first safe idle boundary it captures current contents,
then appends the late batch before any later queued user prompt dispatches.

Combined session-plus-default apply is one server-owned recoverable operation. The TUI
does not sequence plan saving, capture, session mutation, or recovery. It reports the
session target and project-default target independently. Partial completion is never shown
as combined success.

Queued, applying, recovery-required, succeeded, failed, and canceled operations remain
inspectable. Closing the visual editor releases its lease but does not cancel a durable
operation unless the user explicitly requests cancellation while it is still cancelable.

## Later file changes

Before each later real user turn, Jcode observes every captured file under the same path,
external-target, support, stability, and size rules used for capture.

Possible current states are:

- `current`
- `changed`
- `missing`
- `unreadable`
- `unsupported`

Observation does not run for tool-result continuations, provider retries, automatic or
system follow-ups, rendering, or background-only activity.

A current file emits nothing. A new stale state updates the durable receipt and may append
a hidden warning immediately before the real user prompt. The original startup snapshot
remains immutable and work continues.

Each file may add at most two stale-marker messages per session. Repeating the same last
reported stale state creates no duplicate prompt noise. Returning to current clears the
visible stale warning but does not reset the lifetime count. After the two-marker limit,
later observations continue updating receipt truth without adding messages.

Jcode does not offer a direct current-version injection action. The agent can use its
ordinary read tool when the task truly requires current disk contents.

## Caller and session lifecycle

| Caller or lifecycle | Behavior |
|---|---|
| Fresh shared-server TUI session | Capture the active default with blocking semantics before use. |
| Missing-target TUI fallback | Create a fresh primary session and capture before use. |
| `jcode run` new session | Capture before provider use. Failure is typed, nonzero, and makes zero provider calls. |
| Direct REPL new session | Capture before provider-bound input. A block prints every issue and makes zero provider calls. |
| Harness API create | Capture before successful registration. Failure is typed and leaves no orphan session. |
| Resume, reconnect, takeover, reload | Restore existing receipt and transcript without recapture. |
| Harness API attach | Attach to existing state without recapture. A missing target is rejected, not replaced by a fresh session. |
| Split or fork | Inherit the complete transcript and receipt without reinjection. |
| Clear | Create a new primary context and capture again before replacing the old session. |
| Transfer | Create a new reduced primary context, capture again, then append the transfer handoff. |
| Internal Swarm, debug, ambient, scheduled, overnight, and headless work | Explicitly disabled in this phase. A later owner must opt in deliberately. |

Historical sessions without a Startup Context receipt retain their existing behavior. They
never acquire the current project default retroactively on resume.

## Cache behavior

Startup Context is ordered to preserve the longest unchanged provider prefix that current
provider framing permits:

- Stable startup control and ordered file messages precede dynamic session facts.
- The control tag intentionally has no `file_count` attribute.
- Startup control, file, late-update, and stale-marker messages carry no provider-visible
  per-session timestamp text. Lifecycle times live in the receipt.
- Rebuilding an unsent changed suffix preserves the stable control and unchanged file
  prefix.

Startup file prose remains user-authority material. It is not inserted into Jcode's system
prompt. Future prompt-composition work must preserve that authority and cache contract.

## Remote History and exact inspection

`Session.messages` remains the authoritative source, but ordinary remote History is a
privacy-aware presentation projection. It structurally recognizes receipt-owned control,
file, late-update, and stale-marker message IDs and omits their bodies.

The remote TUI receives bounded receipt metadata separately. Exact captured content crosses
to the authenticated client only after an explicit bounded detail request. Detail lookup
validates session, batch, file, message ID, expected digest, and character offset. Repeated
Unicode-safe chunks can reconstruct the complete captured file.

Current-file preview and captured receipt detail are different products:

- Current preview is bounded mutable disk state and is never an apply source.
- Receipt detail is immutable authoritative session content.

## Exports and privacy

Every public exporter starts from a cloned Session and applies one structural Startup
Context policy before formatting.

The default policy is receipt-only:

- Raw control, file, late-update, and stale-marker bodies are omitted.
- Omission records retain approved path and receipt provenance, including resolved path,
  digest, bytes, ordinal, batch kind, and delivery state.
- Omission records are rendered as system information, not fabricated user prompts.
- The authoritative Session is not mutated or persisted in projected form.

The default applies to raw Session export, Markdown conversation export, timeline export,
interactive replay, single-session video, and swarm replay or video paths.

The public replay CLI exposes the explicit full-content route:

```bash
jcode replay <session> --export --include-startup-context
```

The flag also applies to interactive and video replay. Jcode prints a privacy warning to
stderr, keeps machine-readable stdout clean, includes complete Startup Context bodies,
and then applies the existing secret-pattern redaction. Full-context export does not mean
unredacted export.

Project plans, ordinary History, status events, debug summaries, lifecycle logs, and
ordinary protocol diagnostics never contain raw Startup Context file bodies.

## Context Editor integration

Startup Context uses no second transcript and no Startup Context-specific context
projection.

`/context` and `/context edit` read authoritative `Session.messages`, not remote History or
export projection. Startup control, file, late-update, and stale-marker messages are
available by stable message ID and exact content block. A normal explicit range summary
can include them.

Applying a context summary changes only the provider-facing context view. The original
Startup Context messages and receipt references remain byte-identical. Revert and reapply
use the normal context transaction lifecycle, provider validation, revision, continuation
reset, and cache semantics.

See [`CONTEXT_CONTROL.md`](CONTEXT_CONTROL.md) for the full context-control model.

## Private storage and recovery

Under the default durable state directory, project plans live at:

```text
~/.jcode/state/startup-context/projects/<project-key-sha256>.json
```

The JSON stores the complete project identity so load verifies the hashed filename. It
contains paths, approvals, revision, and timestamps, never captured contents. Existing
secret-file storage primitives provide owner-only permissions, durable temporary writes,
fsync, backup, and atomic replacement.

- Missing plan state means an empty default.
- A corrupt primary with a valid backup recovers and rewrites the primary.
- A corrupt primary and backup blocks rather than silently becoming empty.
- Unknown schema versions fail visibly.

Live editor ownership uses project-scoped state under:

```text
~/.jcode/state/startup-context/editor-leases/
```

Only one editable lease may exist per project across sessions and named server processes
sharing the durable state directory. On Unix, a kernel-held `flock` is authoritative.
Disconnect, explicit close, expiry, process exit, and exec reload release ownership. Safe
owner metadata makes contention understandable but does not replace the live lock.

Recoverable apply records live under:

```text
~/.jcode/state/startup-context/apply-transactions/
```

They are owner-only and idempotent. Temporary captured material may exist only for the
bounded recovery lifetime. Terminal resolution removes captured recovery material and
retains only compact operation truth needed for duplicate replay.

## Troubleshooting

| Symptom | Meaning and recovery |
|---|---|
| `Startup context: none` | The active project default is empty. No Startup Context text was injected. Open `/startup` to select files if desired. |
| Initial file issue | Provider dispatch is blocked. Open `/startup`, remove, replace, repair, or approve the exact target, then apply. |
| Project identity or plan storage error | Jcode could not create a truthful receipt. Repair the path or private state problem, then reopen or create a new primary context. |
| Request exceeds safe context budget | Remove files or switch to a suitable model, then manually resubmit the preserved prompt. Jcode never auto-removes or auto-resends. |
| Editor busy | Another live client or server owns the project lease. Close that editor or wait for disconnect, expiry, process exit, or recovery. |
| External target changed | Confirm the new exact resolved target through a fresh preview. Prior approval is not reused. |
| Queued apply | The agent is busy. Jcode will capture at the next safe idle boundary before a later prompt. |
| Recovery required | Inspect the separate session and project-default target states. Retry or query status without inventing a new operation unless the prior operation is terminal. |
| Stale receipt warning | The immutable startup snapshot remains available. Read the current file ordinarily only when the task depends on it. |
| Metadata repair | Provider acceptance occurred, but receipt delivery metadata needs durable repair. Do not interpret this as undelivered source. |
| Direct REPL blocked after disk repair | Use `clear` to create and recapture a new primary context before sending work. |

## Architecture and module ownership

The implementation deliberately has one owner per invariant:

- `jcode-session-types::startup_context`: stable persisted receipt, plan-path, issue,
  delivery, observation, and pending-operation data contracts.
- `jcode-base::startup_context`: project identity, plan storage, selection normalization,
  browsing, complete capture, external-target policy, and observation.
- `jcode-base::session::startup_context`: authoritative message construction, receipt
  installation, dispatch, acceptance, late apply, observation transaction, and persistence
  rollback.
- `jcode-base::session::export`: structural receipt-only and full-content export policy.
- `jcode-app-core::agent::startup_context`: explicit caller activation, blocking policy,
  provider-dispatch preparation, and direct primary-caller behavior.
- `jcode-app-core::server::startup_context`: one shared coordinator for protocol status,
  browsing, detail, leases, apply, queueing, and recovery.
- `jcode-protocol::startup_context`: bounded request, event, status, lease, browser, preview,
  apply, and failure views.
- `jcode-tui` Startup Context UI and editor modules: presentation, local draft state,
  responsive rendering, input routing, and exact response correlation.
- Harness API bridge and SDKs: typed creation failure translation and capability discovery.
- Root replay CLI and app-core replay: public export-policy selection and formatting.

The server owns policy. The TUI sends intent-level requests and never resolves project
identity, validates server files, expands directory rules, hashes content, mutates the
Session, or sequences durable apply stages.

## Future ownership boundaries

The following integration points are intentionally explicit:

- **Phase 3, prompt composition:** decide the future placement of stable interpretation
  guidance while preserving user-message authority, timestamp exclusion, and startup-file
  cache order.
- **Phase 4, isolated sub-agents:** decide whether and how an isolated sub-agent opts into
  Startup Context. A working directory alone must not activate it.
- **Phase 9, unattended execution:** select the typed `InjectDiagnostic` failure policy for
  appropriate scheduled or asynchronous callers and author unattended diagnostic prose.
  Do not inherit interactive blocking accidentally.
- **Phase 10, maintenance:** preserve project-plan storage, receipt schemas, cross-process
  editor ownership, recovery cleanup, export privacy, and the verification matrix during
  upstream stewardship.

## Maintainer change checklist

Before changing Startup Context:

1. Preserve `Session.messages` as the sole authoritative transcript.
2. Keep project defaults path-only and private.
3. Keep one capture and observation policy owner in `jcode-base::startup_context`.
4. Keep initial replacement limited to pre-dispatch state.
5. Keep late history append-only and queue capture at the safe idle boundary.
6. Preserve exact caller activation and lifecycle semantics.
7. Keep stable control and unchanged file prefixes byte-stable when semantics are
   unchanged.
8. Recognize receipt-owned messages by structural IDs, never approved prose or XML shape.
9. Keep ordinary History, status, logs, diagnostics, plans, and default exports body-free.
10. Keep exact content behind explicit detail or warned full-export routes.
11. Verify prompt-safe restoration and no automatic resend after blocked preflight.
12. Verify context summaries, revert, and reapply without source or receipt mutation.
13. Run the focused and integration commands in
    [`dev/STARTUP_CONTEXT_ACCEPTANCE.md`](dev/STARTUP_CONTEXT_ACCEPTANCE.md).
