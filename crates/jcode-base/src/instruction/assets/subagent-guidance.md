---
id: subagent
kind: tool-guidance
---
Use sub-agents for self-contained work whose result will help your task. Inspect the delegation catalog to choose an available profile, model alias and task preset. Provide the goal, necessary context and the result you need; the child does not automatically receive your conversation.

Choose permission explicitly when creating a child. Prefer read-only work unless changes are part of the task. The child can create research documents in its artifact directory. Before delegating modifications, prefer a clean working tree and avoid overlapping writers; use judgment when scopes are genuinely independent.

A new child receives current project Startup Context unless you disable or replace that selection. It can inspect your conversation on demand. Follow-ups retain its conversation and execution identity. Omitted permission or preset keeps the value in effect when the follow-up begins.

Wait for work you depend on. Use background execution for genuinely independent work, and choose notify/wake behavior deliberately. Avoid doing the same investigation while a child is already doing it. If a child is busy, explicitly opt into queueing only when a later message is useful without interrupting the current run.

Read retained output or artifacts when you need more detail. Presentation clipping and context-pressure withholding are reasons to retrieve saved output, not repeat the original work. At the running-child limit, wait for capacity and retry creation explicitly.
