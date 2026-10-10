# Session work

Session work is the server-owned record of one session's own work: its workflow, and in later
stages its stops, questions, session proposals, closeout and timing. This page describes the
foundation that exists today: the store, the workflow file, module types, workflow templates and
which sessions take part.

**Availability.** `features.session_work` (default `false`, environment override
`JCODE_SESSION_WORK_ENABLED`) decides whether *new* sessions take part. While it is off nothing
changes: no session is activated, no store is created, no Session Context line is written and
native file tools refuse the session-work directory like any other harness state. A session's
activation is fixed when it is created and never changes afterwards, whatever the flag later
says. The tools that use this foundation (stop declarations, questions, session proposals) are
not shipped yet; until they are, the flag stays off on ordinary runtimes.

## Which sessions take part

Activation happens while a session is created, before its Session Context is written.

| Creator | With the flag on |
|---|---|
| New session from an attached TUI (`subscribe`) | Activated, no workflow |
| Runtime Clear of a hosted session | Activated, no workflow |
| Runtime Transfer of a hosted session | Activated; the source's workflow head becomes revision 1, with provenance |
| Runtime Split of a hosted session | Continues the source's activation (even with the flag off), without its workflow |
| Isolated child (`subagent`) | Activated as a child; its task preset's workflow template, if any, becomes revision 1 |
| Harness/SDK sessions, `primary_launch`, `jcode run`, REPL, process-owned TUI Clear/Transfer/Split, scheduled and debug sessions | Not activated |

Resume, reload and restart keep everything: the binding is saved with the session. A session
proposal (when it exists) never copies a workflow.

An activated session's Session Context ends with one line naming its workflow file and the
managed global `session-work` skill (`~/.jcode/instructions/skills/session-work/SKILL.md`). A
split, whose copied transcript names its source's file, gets its own line in the fork notice.

## Storage

- **Store:** `<durable state>/session-work/store.sqlite3` (`$JCODE_RUNTIME_DIR/durable-state` when
  set, else `~/.jcode/state`). Owner-only file and directory, WAL, `synchronous=FULL`, schema
  version 1. It is the authority for workflows. An unreadable, damaged or unknown-schema store is
  never treated as empty: an activated session's turn stops with a visible error before its next
  provider request, and its session-work files cannot be written, until the store is repaired.
  Sessions without session work are unaffected.
- **Tables:** activation (role, origin, frozen module types), items and aliases, workflow
  revisions, workflow state (head and the revision last written to the file), module times, and
  the permanent `events` and `journal` records. Every mutation is keyed by a request ID derived
  from the tool invocation, so a replay converges on its original result.
- **Files:** `~/.jcode/session-work/<session>/` (owner-only) holds `workflow.md`, `summary.md` and
  `history/rNN.md`.

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

## Recovery and cleanup

- A session that fails before publication removes its store rows and directory with its other
  unpublished state. Permanent `events` and `journal` rows are never removed.
- The store keeps the head revision and the revision last written to the file, so a crash between
  them is completed at the next safe point without a notice.
- A replayed tool invocation converges on the revision it committed.

## Not yet implemented

Stop declarations and their corrections, arrivals and holding, timers, questions, session
proposals, timing reports, closeout and the attention stream build on this foundation. The
`session-work` skill named in the Session Context line is installed together with the stop
protocol.
