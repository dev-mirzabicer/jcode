/// A parsed `/plan` command. Planning is a one-shot, plan-only action: it never
/// edits files and is not a resumable loop like `/improve` or `/refactor`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PlanCommand {
    pub goal: Option<String>,
}

pub(super) fn parse_plan_command(trimmed: &str) -> Option<PlanCommand> {
    let rest = trimmed.strip_prefix("/plan")?;
    // Only treat `/plan` and `/plan <goal>` as a plan command, not `/planfoo`.
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let goal = rest.trim();
    Some(PlanCommand {
        goal: if goal.is_empty() {
            None
        } else {
            Some(goal.to_string())
        },
    })
}

pub(super) fn plan_launch_notice(goal: Option<&str>, interrupted: bool) -> String {
    let prefix = if interrupted {
        "👉 Interrupting and planning"
    } else {
        "🧭 Planning"
    };
    let notice = match goal.map(str::trim).filter(|goal| !goal.is_empty()) {
        Some(goal) => format!("{} {}... (plan-only; no edits)", prefix, goal),
        None => format!("{}... (plan-only; no edits)", prefix),
    };
    format!(
        "{}\nNote: It is better to talk with your agent until it understands what you mean than to make a plan too early.",
        notice
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plan_accepts_bare_and_goal_forms() {
        assert_eq!(
            parse_plan_command("/plan"),
            Some(PlanCommand { goal: None })
        );
        assert_eq!(
            parse_plan_command("/plan   "),
            Some(PlanCommand { goal: None })
        );
        assert_eq!(
            parse_plan_command("/plan add a compact mode"),
            Some(PlanCommand {
                goal: Some("add a compact mode".to_string())
            })
        );
    }

    #[test]
    fn parse_plan_rejects_other_commands() {
        assert_eq!(parse_plan_command("/planner foo"), None);
        assert_eq!(parse_plan_command("/improve"), None);
        assert_eq!(parse_plan_command("plan ahead"), None);
    }

    #[test]
    fn launch_notice_encourages_conversation_before_planning() {
        let notice = plan_launch_notice(Some("ship feature x"), false);
        assert!(notice.contains("Planning ship feature x"));
        assert!(
            notice.contains(
                "It is better to talk with your agent until it understands what you mean"
            )
        );
    }
}
