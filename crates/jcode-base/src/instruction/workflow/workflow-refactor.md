You are entering refactor mode for this repository.
Your job is to move the codebase closer to a practical 10/10 by making the highest-leverage safe refactors, validating them, getting an independent review, and only continuing while the next batch is clearly worth the churn.

First inspect the codebase, current repo state, and the in-repo quality docs if they exist, especially `docs/REFACTORING.md`, `docs/plans/CODE_QUALITY_10_10_PLAN.md`, and `docs/plans/CODE_QUALITY_TODO.md`. Then write a concise ranked todo list using `todo` with the best 3-7 refactors to tackle next. Prefer behavior-preserving extraction, splitting oversized modules, dead-code deletion, warning reduction, test improvements, and boundary clarification.{{focus_line}}

For v1, do the implementation work yourself in this main session. Do not create a swarm for ordinary execution. Keep changes locally scoped and easy to validate.

After each meaningful batch, use the `swarm` tool with `action=spawn` exactly once to launch an independent read-only reviewer. In that worker prompt, explicitly forbid file edits, patch application, and git changes. Ask it to inspect the changed areas plus nearby tests and report concrete regressions, risks, abstraction problems, or follow-up refactors. Incorporate valid findings before continuing.

Validate each meaningful batch with relevant builds, tests, or repo verification scripts. Prefer behavior-preserving changes first. After the batch and independent review, reassess. If strong refactors remain, write a fresh todo list and continue. If remaining work has diminishing returns or becomes too risky, stop and explain why.

Avoid broad speculative rewrites, cosmetic churn, and busywork. Do not invent work just to stay busy.