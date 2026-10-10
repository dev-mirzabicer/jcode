---
id: session-work
kind: skill
name: session-work
description: How a session plans its work, pauses and finishes.
disable-model-invocation: true
---
# Session work

How a session plans its work, pauses and finishes.

## Workflow

For a substantial session, keep a workflow: the meaningful parts of the session, in order, revised as understanding grows. Trivial sessions skip it. Todos stay your short-term checklist; the workflow is the session's shape.

The file is named in Session Context. Write and edit it with your file tools. The host checks every edit and rejects an invalid one with line numbers.

    - [x] ground: Understand the sidebar and its constraints {research}
    - [>] proto: Prototype three sidebars {prototype/frontend}
      - [x] proto-a: Minimal rail
      - [>] proto-b: Dense panel
    - [?] after: Decide the rest after Mirza picks one
    - [-] perf: Performance pass
      > skipped: CSS-only change

Markers:
- `[ ]` pending
- `[>]` active
- `[x]` done
- `[-]` skipped, with `> skipped: <reason>`
- `[?]` undetermined: decide when you get there

Lines are `id: title` with an optional `{module type}`. Two spaces nest a child, and `>` lines are notes. One module is active at a time, and a parent's marker mirrors its children.

`module-types.md`, next to the workflow file, lists this session's module types. Use one that fits, or a short label of your own.

Leave settled modules as they are unless they truly need a correction. `history/` holds older revisions; writing one back undoes shape changes. A workflow stays once created: skip the modules you drop and give the reason.

## Stopping

Without a workflow, ending your turn completes it. Once a workflow exists, declare every stop with declare_stop, then write your final message:
- complete: the workflow is finished. This is also how the workflow is marked complete.
- wait: you are pausing for something.

## Waiting, waking and holding

Everything that can finish later gets an alias in the result that creates it:
- `t`: background task
- `c`: background sub-agent run
- `q`: question
- `a`: questionnaire
- `p`: session proposal decision
- `p<N>.done`: that launched session's declared completion
- `r`: scheduled resume

Each item has a policy, set when it starts: `wake` or `hold`. bash, bg and subagent take `wake: true` or `false`; ask_user takes `on_answer`.
- While you wait, only your condition wakes you. Everything else is held and arrives, oldest first, together with whatever wakes you.
- After completion, `wake` items start a new turn and `hold` items wait for your next one.
- A wait's time limit never stops anything. When it passes, you get a check-in with the state of everything still pending.

## Closeout

When the session's work is finished and recorded, write `summary.md` next to the workflow file and declare complete with `closeout: true`. Follow-up questions afterwards need no reopening. To change the workflow after closeout, add `> reopened: <reason>` in your first workflow edit.
