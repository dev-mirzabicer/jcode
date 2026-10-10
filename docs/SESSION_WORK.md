# Session work

Session work is the server-owned record of one session's own work: its workflow and its stops, and
in later stages its questions, session proposals, closeout and timing. This page describes what
exists today: the store, the workflow file, module types, workflow templates, the stop protocol
and which sessions take part.

**Availability.** `features.session_work` (default `false`, environment override
`JCODE_SESSION_WORK_ENABLED`) decides whether *new* sessions take part. While it is off nothing
changes: no session is activated, no store is created, no Session Context line is written and
native file tools refuse the session-work directory like any other harness state. A session's
activation is fixed when it is created and never changes afterwards, whatever the flag later
says. Questions, session proposals and the human client that answers them are not shipped yet;
until they are, the flag stays off on ordinary runtimes.

## Which sessions take part

Activation happens while a session is created, before its Session Context is written.

| Creator | With the flag on |
|---|---|
| New session from an attached TUI (`subscribe`) | Activated, no workflow |
| Runtime Clear of a hosted session | Activated, no workflow |
| Runtime Transfer of a hosted session | Activated; the source's workflow head becomes revision 1, with provenance |
| Runtime Split of a hosted session | Continues the source's activation (even with the flag off), without its workflow |
| Isolated child (`subagent`) | Activated as a child; its task preset's workflow template, if any, becomes revision 1 |
| `primary_launch` with `session_work: true` (Harness/SDK opt-in; `/workspace` launches opt in) | Activated, no workflow |
| Other Harness/SDK sessions, `primary_launch` without the opt-in, `jcode run`, REPL, process-owned TUI Clear/Transfer/Split, scheduled and debug sessions | Not activated |

Resume, reload and restart keep everything: the binding is saved with the session. A session
proposal (when it exists) never copies a workflow.

An activated session's Session Context ends with one line naming its workflow file and the
managed global `session-work` skill (`~/.jcode/instructions/skills/session-work/SKILL.md`). A
split, whose copied transcript names its source's file, gets its own line in the fork notice.

**Launch opt-in.** `PrimaryLaunchInput.session_work` (omitted when false) asks a hosted launch to
take part; the runtime's flag still decides. The `primary_launch_capabilities` reply carries
`session_work_opt_in: true` on runtimes that accept the field, and both SDKs refuse to send the
opt-in to a runtime that does not advertise it. A launch that opts in but cannot be activated is
removed before publication and fails.

**Tool surface.** An activated primary gets `declare_stop` with `wait`, `complete` and
`withdraw`; an activated child gets it with `complete` only. Sessions without session work never
see it. In activated sessions the `wake` parameter of `bash`, `bg` and `subagent`, the `timeout`
of `bash`, and `max_wait_seconds` and `task_id` of `bg` carry session-work wording (until the
arrival and timer stages land, the behavior those words describe is not complete, which is one
reason the flag stays off). All of this text is managed (seed 33) and frozen into the session at
its first successful request, like the other managed tool guidance: a session keeps the text it
started with, and new sessions take later edits.

## Storage

- **Store:** `<durable state>/session-work/store.sqlite3` (`$JCODE_RUNTIME_DIR/durable-state` when
  set, else `~/.jcode/state`). Owner-only file and directory, WAL, `synchronous=FULL`, schema
  version 2 (a version 1 store is upgraded in place when opened). It is the authority for
  workflows and stops. An unreadable, damaged or unknown-schema store is
  never treated as empty: an activated session's turn stops with a visible error before its next
  provider request, and its session-work files cannot be written, until the store is repaired.
  Sessions without session work are unaffected.
- **Tables:** activation (role, origin, frozen module types), items and aliases, workflow
  revisions, workflow state (head and the revision last written to the file), module times, stop
  declarations, per-turn counters, turn outcomes, and the permanent `events` and `journal`
  records. Every mutation is keyed by a request ID derived
  from the tool invocation, so a replay converges on its original result.
- **Files:** `~/.jcode/session-work/<session>/` (owner-only) holds `workflow.md`, `summary.md`,
  `history/rNN.md` and `module-types.md`. `module-types.md` (read-only, 0400) lists the module
  types frozen into the session; it is generated from the store and rewritten silently at the next
  safe point if it is changed or removed. Native tools refuse to write it.

## The workflow file

The agent writes and edits `workflow.md` with its ordinary native file tools (`write`, `edit`,
`multiedit`, `patch`, `apply_patch`). These tools route the session's own session-work directory
through a dedicated destination:

1. The complete new text is parsed and validated. An invalid text changes nothing; the tool fails
   with line-numbered errors.
2. One store transaction appends the revision and updates module times.
3. The file is written (atomically) and `history/` is refreshed with the newest 40 revisions as
   read-only files. Every revision stays in the store.

Reads through these tools return the store's text. Deleting or moving a session-work file,
writing `history/`, writing any other name, and writing another session's directory are refused
before any effect. A symlink placed at `workflow.md` is replaced, never followed. `summary.md` is
a plain draft the agent may write; closeout will freeze it.

A workflow exists once its first revision is committed and is never deleted. Undo means writing
an older revision's text back.

**Changes outside the tools.** Before each provider request, both turn loops compare the file
with the store. A file whose last write was interrupted is completed silently. A file changed or
removed by anything else (a shell command, an editor) is restored to the head revision, and the
session receives one appended notice on the `SessionWork` context-delivery channel (operator
authority, like other harness notices; see [notifications](NOTIFICATIONS.md)).

### Grammar

```text
- [x] ground: Understand the sidebar and its constraints {research}
- [>] proto: Prototype three sidebars {prototype/frontend}
  - [x] proto-a: Minimal rail
  - [>] proto-b: Dense panel
- [?] after: Decide the rest after Mirza picks one
- [-] perf: Performance pass
  > skipped: CSS-only change
```

- Module lines are `- [marker] id: title`, with an optional `{type}` or `{type/subtype}`.
- Markers: `[ ]` pending, `[>]` active, `[x]` done, `[-]` skipped, `[?]` undetermined.
- IDs use lowercase letters, digits, `-` and `_`, start with a letter or digit, and are unique
  in the workflow.
- Two spaces nest a child. Notes are `>` lines directly under a module, indented two spaces
  deeper. A skipped leaf needs a `> skipped: <reason>` note.
- A parent's marker must equal the status derived from its children: active if any child is
  active; done when all are finished and not all skipped; skipped when all are skipped;
  undetermined when all are undetermined; pending otherwise.
- At most one leaf is active. A workflow has at least one module.
- Blank lines and CRLF line endings are accepted. The store keeps the exact text written.

### Module times

The store records when each module first became active and when it finished (done or skipped),
outside the file. A module that leaves its finished state clears its finish time.

## Module types

Module types are a managed instruction resource kind in `module-types/` of the global or project
instruction store, edited through the instruction manager like other resources:

```markdown
---
kind: module-type
id: research
name: Research
description: One line on what the module is for
subtypes:
  - web
  - code
skill: global:research
---
Optional body.
```

`name` and `description` are required; `subtypes` are slugs; `skill` is an optional skill
selector. Project definitions take precedence over global ones with the same ID; invalid
definitions are left out. The effective list is frozen into each session when it is activated, so
later edits reach new sessions only. A split keeps its source's list. A `{type}` label in the
workflow is free text: the registry describes types, it does not restrict them. Jcode ships no
module types; their content belongs to the agent guidance.

## Workflow templates

A task preset (`notifications/task-preset.<id>.md`) may carry a workflow template in its
frontmatter:

```markdown
---
id: task-preset.review
kind: notification
workflow: |
  - [>] read: Read the change
  - [ ] report: Report findings
---
Preset text.
```

The template is validated with the workflow grammar when the resource is loaded; a preset with an
invalid template is invalid. `workflow` is accepted only on task presets. A child created with the
preset starts with the template as revision 1 (source: the preset).

## Stop protocol

Once an activated session has a workflow, every voluntary end of a turn is declared with
`declare_stop`, then the agent writes its final message. Without a workflow, or once the workflow
was declared complete at its current revision, a turn that simply ends completes implicitly. A
workflow is completed only by a declaration; all modules done is not enough.

**Declarations.** A declaration is validated against current state before it is stored:

- `wait` names a condition over the session's item aliases (`q1`, `any(q1, t3)`, `all(t3, t4)`,
  nested), a one-line `summary` and an optional `timeout` (`45m`, `2h`, `none`). Every alias must
  belong to the session, and a condition that finished items already satisfy is refused. Without a
  timeout, the default is composed through the condition from `[session_work.timeouts]`: `any`
  takes the shortest, `all` the longest. The timer itself arrives with a later stage.
- `complete` needs every workflow module done or skipped, and no running background work except
  items named in `leave_running`. `summary`, `closeout`, `documents` and `accepted` are recorded
  (closeout itself arrives with a later stage). Children may declare only `complete`.
- `withdraw` clears the current declaration and gives back the turn's warned call.
- Declaring again replaces the current declaration. A repeated invocation converges on its
  original record.

Running background tasks and sub-agent runs get their task or child alias, and their wake/hold
policy, the first time session work sees them; the alias and policy never change afterwards.

**Tool gate.** While a declaration governs the turn (one made in it, or a wait in effect), the
first other tool call runs and its result carries a warning; later calls are refused with a tool
result that says so. `batch` is refused outright, so none of its calls run. `declare_stop` always
runs. Each turn has its own warned call and refused-call counter. When the counter runs out the
turn ends: the declaration stays and takes effect if it still holds.

**End of a turn.** At the real end of a turn (no tool calls and nothing left to inject):

| Situation | Result |
|---|---|
| A declaration that still holds | It takes effect with the final message: waiting, or completed |
| A declaration that no longer holds | It is invalidated and corrected, while the invalid counter lasts |
| No declaration, no workflow or a workflow declared complete | Completed implicitly |
| No declaration and an unfinished workflow | Corrected, while the missing counter lasts |
| Provider refusal, truncated or empty output | Recorded as such, never corrected; a declaration made in the turn survives |

Corrections are appended as operator-authority context deliveries on the `StopCorrection`
channel ([Notifications](NOTIFICATIONS.md)), once per occurrence, and the turn continues. Earlier
transcript content is never rewritten. `[session_work.corrections]` sets the per-turn counters
(`missing = 2`, `invalid = 3`, `refused = 2` by default): each counts the corrections or refusals
given, and the next occurrence ends the turn as "stopped without a declaration", which the client
shows. Counters are stored per turn, so a reload or crash in the middle of a correction continues
with the same counters.

**Outcomes.** Every turn of an activated session records one outcome: completed (declared or
not), waiting, interrupted by the user, interrupted by reload, provider refusal, truncated, failed
(provider, preflight or other) or stopped without a declaration. A failure keeps a declaration
made in its turn (it takes effect if it still holds). The user's Stop revokes a declaration made in
the stopped turn; a wait already in effect stays. A reload leaves the turn open for its
continuation. How later arrivals wake or hold a waiting session belongs to the next stage; until
then any new input starts a turn in which an effective wait still gates tool calls.

**Cost.** A declared stop costs one more provider request (the final message after the
declaration's result). Tool definitions and guidance stay frozen; corrections and notices are
appended, so the earlier prefix stays cached.

## The session-work skill

The managed global skill `skills/session-work/SKILL.md` (seed 33) explains workflows, stopping,
waiting and closeout. It has `disable-model-invocation: true`: it is not listed in any session's
available-skills catalog or in `skill_manage list`, so sessions without session work are never
told about mechanisms they lack. Activated sessions are pointed at it by their Session Context
line and read it by path or name; a person can still invoke it with `/session-work`.

## Recovery and cleanup

- A session that fails before publication removes its store rows and directory with its other
  unpublished state. Permanent `events` and `journal` rows are never removed.
- The store keeps the head revision and the revision last written to the file, so a crash between
  them is completed at the next safe point without a notice.
- A replayed tool invocation converges on the revision it committed.

## Not yet implemented

Arrival rules and holding, wait timers and time checks, questions, session proposals, timing
reports, closeout and the attention stream build on this foundation.
