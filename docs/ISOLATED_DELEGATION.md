# Isolated child conversations

`get_catalog` and `subagent` provide isolated, durable conversations without Swarm
coordination. A child uses the ordinary Agent loop, retained tool outputs and
Session history. Its original parent owns follow-ups and control.

## Parent workflow

1. Call `get_catalog` to discover usable profile, model-alias and task-preset
   selectors. Descriptions come from their own resources, not a second catalog.
2. Call `subagent` with a self-contained task and explicit `agent`, `model_alias`
   and `permission`. The child does not automatically receive parent history.
3. Receive one ordinary tool result, or explicitly request background execution.
4. Continue that conversation with `child_id`. Read its artifacts or use
   `session_outline`, `read_transcript` and `expand_tool_use` for more detail.

Example creation arguments, in addition to the normal display `intent`:

```json
{
  "agent": "global:jcode",
  "model_alias": "researcher",
  "permission": "read_only",
  "prompt": "Inspect the parser's handling of invalid UTF-8. Return the relevant code paths and evidence. Do not modify project files."
}
```

A follow-up supplies `child_id` and `prompt`. Optional `permission` and `preset`
changes apply when its turn begins. Profile, concrete model/effort, working
folder, initial Startup Context and MCP blocklist remain fixed. Creation-only
fields on a follow-up are rejected rather than silently ignored.

## Independent selections and instructions

- **Agent:** required, with isolated or both availability. Unqualified selectors
  use existing project-first specificity. Invalid specific sources do not fall
  through to a different profile.
- **Model alias:** required. Existing roster resolution constructs an independent
  provider. It never temporarily changes the parent's provider or effort.
- **Effort:** optional at creation. Omission uses the alias default, then the fresh
  provider/model default. It does not mean literal `none`.
- **Preset:** defaults to `general`. Presets are notification resources named
  `task-preset.<id>`, with the normal global/project qualification and modules.

The true system prompt contains the normal selected profile composition followed
by the managed `system/subagent.md` addition. Presets are complete persisted
user-authority notices before the task, including the initial preset. They never
rewrite the system prefix. Omitted or unchanged presets preserve their existing
notice. A changed identity renders current source at actual turn start. The
intentionally empty general preset means no specialization.

Current permission and preset notice IDs are structural Session state. Existing
context preview, curator capture and commit checks protect them. Rewind pins
current notices, and changed instructions invalidate incompatible undo state.
Superseded notices remain ordinary history. This reuses the Context Editor and
projection owner, not a second compaction system.

The shipped common-child and parent tool guidance are framework-stage resources.
They are editable through the instruction system. The final role/skill corpus is
separate. Tests use synthetic content to verify mechanisms, not prose quality.

## Startup Context

Creation captures latest file contents from the selected project's saved plan.
`startup_files` is an ordered child-only replacement, including an explicit empty
list. Relative custom paths resolve from the child working directory. Readable
external files may be selected by the parent through the existing capture and
resolved-target validation engine, without another human approval.

`disable_startup_context: true` captures nothing and conflicts with any supplied
list. Capture failures reject preparation. Existing complete-source and provider
context bounds remain rejection boundaries, not clipping. Saved project defaults
are not changed. Follow-ups retain the original capture and use existing bounded
staleness observations rather than overwrite it.

## Permissions and MCPs

`read_only` permits known native file mutations only inside the child's dedicated
artifact directory. `write`, `edit`, `multiedit`, unified/Codex patch operations
and their aliases validate destinations before effects. Multi-target patches and
moves check every destination. Symlink/traversal attempts cannot silently rebind
artifact permission outside its stored directory.

`read_write` removes the artifact-only restriction for ordinary task files.
Durable harness state remains protected. Jcode scratch projects are task files,
not global configuration. This is narrow enforcement plus instructions, **not an
adversarial OS sandbox**. Shell, computer-use and external tools retain their
normal capabilities and trust boundaries. Completed filesystem effects are not
rolled back on Stop or failure.

Children cannot recursively delegate or perform harness-wide administration,
reload Jcode, schedule other agents, or mutate project/global initiatives. Their
own ordinary todos remain available. The parent updates shared initiatives after
reviewing the child's result.

MCP discovery and global/project precedence are the same for parents and children.
The effective server definition may declare:

```json
{
  "mcpServers": {
    "documentation": {
      "command": "/absolute/path/to/documentation-server",
      "read_only": true
    }
  }
}
```

`read_only` is explicit server-level policy. Absent/false means non-read-only.
Read-only children automatically exclude non-read-only servers. Read-write
children retain all otherwise eligible servers. Creation's optional
`blocked_mcps` adds exact-name exclusions that persist across follow-ups and
reload. Parents need not list every mutating server when choosing read-only.

MCP connect, reload and direct calls enforce eligibility, including cached tools.
Read-only children cannot spoof another launch command under a classified name.
Session-local management does not rewrite global configuration. Shared processes
are keyed by effective launch identity and working directory, not name alone.
Session reload/disconnect releases only its leases and does not restart unrelated
shared users. Configured `shared: false` clients are fresh child-owned clients.
MCP classification is not proof that an incorrectly classified server is safe.

## Waiting, FIFO and capacity

Foreground is the default, without an arbitrary automatic detach timer. Explicit
background execution defaults to `notify: true`, `wake: false`; callers can
change these through ordinary background delivery controls. User promotion keeps
one execution identity. A background receipt is acceptance, not completion, and
never creates a second tool result for the same tool use.

Busy follow-ups reject unless `queue_if_busy: true`. Accepted intent is persisted
immediately in its original invocation record. FIFO settings activate when each
turn starts, so omission inherits the then-current permission/preset. Submitting
queued input does not alter the running turn. Any successful reply advances the
queue, including a clarification question. The harness does not grade reply prose.

Stopping a running child or terminal failure cancels its unstarted queue while
retaining original input. Cancelling a selected queued entry affects only it.
There is no paused-queue/resume subsystem or separate cancelled-message archive.

```toml
[delegation]
max_running_children = 15
```

Admission is atomic across the execution store. One busy child with a FIFO holds
one reservation. Idle stored conversations have no TTL and consume no running
slot. Capacity overflow rejects without starting work or creating a capacity
queue. No model downgrade is used to obtain capacity.

## Hosting, continuation and recovery

Shared primary sessions execute delegation directly in the shared host. Standalone
`run` and REPL inference stays local; their delegation tools use a dedicated,
version/namespace-checked host connection before local supervision. JSON/NDJSON
stdout remains machine-readable. Replayed transport identity retrieves the same
operation rather than repeat effects.

Children use host configuration and authentication, not copied parent provider
objects, skills or arbitrary environment exports. When delegation autostarts a
missing host, it supplies OS execution and Jcode namespace variables, not arbitrary
caller exports or API keys. Persistent provider/MCP configuration supplies host
credentials. Ordinary TUI server startup is unchanged. Existing hosts keep their
own environment.

Same-parent resume preserves children. Split/transfer descendants can inspect
inherited history but do not gain control or clone children. Clear creates fresh
ownership without deleting old data. Child conversations are not direct human chat
sessions. The task monitor provides a human idle-child Context action using the
existing editor and the same enforced directive protection.

A stopped child becomes available for an explicit follow-up after owned work is
actually quiescent. Capture keeps available partial output. Context exhaustion is
an ordinary failure for parent review and human context repair, not automatic
compaction. Transient transport retries reuse the normal loop. Primary-only
reconsideration/title/review workflows are not adopted implicitly.

Ordinary follow-ups reuse a warm child runtime, including its stateful MCP
connections. The host bounds idle runtime caching by the configured child limit.
Eviction, process restart, explicit permission changes or external context edits
may reconstruct runtime resources, but never expire conversation history or
artifacts. Background tools retain their originating turn's immutable permission
snapshot even after a later permission change or idle runtime eviction.

Restore uses the exact stored roster resolution and frozen instructions without
reading an edited alias or profile. Unavailable concrete routes fail rather than
switch models. Routes that execute their own tools outside host enforcement are
rejected for child execution.

Reload checkpoints hosted children, including background children of idle/local
parents, through the common execution owner. It distinguishes reload interruption
from human cancellation and waits for durable terminal outcomes before replacing
the host. Lost-owner recovery never resends uncertain work. Unstarted queued child
input is cancelled; running work is interrupted only after actual ownership proof.
Still-owned foreground child tools retain their reservation until they stop.

After transport loss or context-pressure withholding, inspect the retained run or
saved output. Do not repeat the creation or side-effecting tool merely to retrieve
its result. Output retention, read points, as-of snapshots and archive lifecycle
remain the shared [execution](TOOL_EXECUTION.md) and
[inspection](SESSION_INSPECTION.md) services.

## Verification and compatibility

Harness v1.5 advertises `isolated_delegation_v1`; current SDK execution types include
queued state and predecessor cancellation. The actual child-host transport
negotiates its own version and namespace before submission. Unsupported hosts
reject without a child launch. macOS arm64 is the native validation target;
platform-specific and external-provider limits remain explicit in acceptance
records. Local fixtures do not establish hosted billing, prompt quality or remote
service termination guarantees.

## Human monitoring and context repair

Use [`/tasks`](TASK_MONITOR.md) for Active/Completed runs, retained input/output,
Stop/background controls and the idle-child Context Editor. Human context editing
uses the existing reversible transaction service. It does not create a child chat
attachment or automatically restart inference.
