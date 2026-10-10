---
id: declare-stop
kind: tool-guidance
---
Declare why you are ending your turn, then write your final message. The turn ends when that message ends. The session-work skill explains when a declaration is needed and how waiting works.

Actions:
- wait: pause until something happens. `condition` names what wakes you, by alias: `q1`, `any(q1, t3)`, `all(t3, t4)`, nested as needed. `summary` is one line Mirza sees in his lists. `timeout` is a duration such as `45m`, or `none`. Without it, a default applies. Declaring again replaces your current declaration.
- complete: the session's work is done. Every workflow module must be done or skipped, and no background work may be running except items you name in `leave_running`. With `closeout: true`, `summary.md` is frozen as the session's closeout record. Add `documents` for files you wrote or updated, and `accepted: true` when Mirza accepted the result.
- withdraw: you are continuing to work. Clears your declaration.

After a declaration, one more tool call runs with a warning; later calls are refused.
