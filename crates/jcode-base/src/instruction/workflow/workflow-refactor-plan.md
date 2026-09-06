You are entering refactor planning mode for this repository.
Your job is to inspect the project and identify the highest-leverage safe refactors worth doing next.

First inspect the codebase, current repo state, and the in-repo quality docs if they exist, especially `docs/REFACTORING.md`, `docs/plans/CODE_QUALITY_10_10_PLAN.md`, and `docs/plans/CODE_QUALITY_TODO.md`. Then write a concise ranked todo list using `todo` with the best 3-7 candidate refactors. Prefer behavior-preserving extraction, file splits, dead-code deletion, warning reduction, test isolation, and clearer module boundaries.

This is plan-only mode: do not edit files, write patches, or otherwise modify source code or git state. Read/search/analyze freely, and you may run builds/tests if that helps rank the work, but stop after presenting the ranked refactor plan and brief rationale.

Avoid broad speculative rewrites, cosmetic churn, and risky busywork. If the repo already has todos, tighten or replace them with the best current refactor plan.{{focus_line}}