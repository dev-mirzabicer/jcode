# Task monitor

`/tasks` opens the native TUI monitor. It inspects the existing execution store and
routes controls to the runtime that owns each invocation. It is not a scheduler,
Swarm view, OS process manager or another conversation store.

## Browse work

The initial **Active** view includes this session and its isolated children. Press
`2` for **Completed**, which includes failed, cancelled and interrupted work, or
`1` to return to Active. `a` explicitly toggles all Jcode sessions in this state
namespace. Merely listing or refreshing tasks does not count as session activity.

Use Up/Down or `j`/`k` to select a row. Enter opens details. Right expands a batch
or a child's execution history, and Escape backs out. Child history is identified
as conversation history, not a claim that every tool belongs to the selected
follow-up. Selection follows the run ID, not its row number. If selected work
finishes, its completed details remain pinned rather than selecting another run.

At 120 columns and wider, the list and selected preview appear together. Standard
80-column and 60-column terminals use one pane with details on Enter. The monitor
supports 48×12 and shows an explicit size message below that floor. Escape remains
available there. The child editor retains its existing layout and controls.

`?` opens a scrollable **Actions** menu. Every monitor action has both a keyboard
route and a visible mouse target there, including actions not shown in a narrow
footer. Click a row or preview to focus it. Tab switches list/output focus in the
wide view. Keyboard and mouse wheel scroll the focused content. Mouse capture can
be bypassed with the terminal's usual Shift gesture for terminal text selection.

## Read input and live output

`i` switches between the immutable invocation input and retained output. `f`
toggles live follow/pause. Scrolling output pauses following. A late response cannot
replace a paused view or the content for a newly selected run. `[` and `]` page
backward and forward. In the list these keys navigate bounded metadata pages, and
in details they navigate the selected text source.

The UI reads bounded, UTF-8-safe windows from the existing verified output owner.
It does not rerun producers, load all output bodies for a list, or assume server
paths are mounted on the client. Cancelled work can have useful partial output.
An offline archive, deliberately deleted output, failed capture or unknown legacy
source is an unavailable/error state, not an empty successful result. Raw binary
parts and full programmatic retrieval remain available through the existing
[execution interfaces](TOOL_EXECUTION.md).

Plain source-read runs expose their existing retained receipt and continuation
metadata, not a second archived copy of the entire source file. Opening that row
does not reread a possibly changed file. Existing session inspection can show the
excerpt that was already delivered in the conversation.

## Stop and background

- `s` sends **Stop** to the selected invocation. Acknowledgement means the owner
  received the request, not that the operation has stopped or persisted its result.
- `b` promotes applicable foreground work to **Background** without starting it
  again or changing invocation identity.
- `S` requests **Force stop** when a verified native process is available. The
  action is conditional in the menu. It signals only the recorded private process
  group through its verified runtime owner. It never kills the shared Jcode server.

Ordinary native Stop already escalates after its grace period. Some external or
legacy work has no stronger supported force action. Unsupported control fails
explicitly. Stopping a background waiter does not stop the separately owned work.
Partial output and completed filesystem effects are retained. No automatic
rollback, stash, compensating service request or child restart occurs.

## Human child Context Editor

Select a child invocation and choose **Child Context Editor** (`c`). The title
identifies the child. This opens the existing [Context Editor](CONTEXT_CONTROL.md),
including its range review, curator workspace, preview, apply, history and undo.
It does not attach a human chat client to that child.

Opening/capture and mutation require an idle child. Short kernel-owned control
leases coordinate with the existing child admission owner. Browsing does not
reserve the child indefinitely. A parent may submit a follow-up while a draft is
being reviewed. The existing context transaction owner rejects busy operations and
revalidates the exact transcript revision, provider identity and active directives
before committing. Current profile, permission and preset messages stay protected.
The parent session's context state and pressure controls remain independent.

Local TUI child operations also use the compatible shared child host. Reconnect
refreshes state without resending an unconfirmed mutation. A lost/expired draft or
changed target is reported explicitly. Applying or undoing a context transaction
never sends a new child message and never automatically restarts inference. The
parent decides whether to continue the child.

## Storage review

Storage is a secondary action (`g`). It shows the last retention scan and any
reported storage issues. Enter a positive byte target to review the oldest whole
completed cold-archived outputs. Live output and recent emergency spillover are
excluded by the existing storage owner.

Enter **reviews**, it does not delete. The review names exact output identities,
selected bytes, whole-output overshoot and affected snapshots. `y` confirms that
review once. Escape returns without confirming. A stale review is rejected, and
partial deletion outcomes identify the outputs that changed. Delivered transcript
text, invocation inputs, project files and child-authored artifacts remain intact.
The trusted same-user client boundary is unchanged. See
[inspection and retention](SESSION_INSPECTION.md) for storage recovery and archive
privacy details.

## Compatibility and failure recovery

The internal TUI negotiates version 1 through `task_monitor_probe` before using the new monitor route.
Old servers are not treated as empty task lists or as supporting child targeting.
SDK force-stop callers separately require `execution_force_stop_v1`; ordinary
execution and binary-part capabilities retain their existing semantics.

When transport or an owner disappears, keep the original invocation identity.
Refresh or reconnect to inspect its current receipt. Do not rerun a side-effecting
tool to recover its output. This is the same recovery rule used by the
[isolated delegation workflow](ISOLATED_DELEGATION.md).

For the maintainer requirement matrix and isolated native verification procedure,
see [task monitor acceptance](dev/TASK_MONITOR_ACCEPTANCE.md).
