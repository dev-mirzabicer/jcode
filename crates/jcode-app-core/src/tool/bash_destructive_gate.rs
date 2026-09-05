//! The destructive-command gate for the `bash` tool (issue #604).
//!
//! Kept in its own file so the policy seam is easy to find and review: this is
//! the only thing standing between a model's `rm -rf` and the user's data.

#[cfg(test)]
#[path = "bash_destructive_gate_tests.rs"]
mod tests;

/// Apply the deterministic destructive-command gate, returning refusal text
/// when the command must not run as-issued.
///
/// Stage 1 is a pure blast-radius assessment; stage 2 turns a `Confirm` verdict
/// into a reflection prompt that a blind retry cannot satisfy. Catastrophic
/// targets (`/`, `$HOME`, credential stores, device nodes) are denied outright.
/// See issue #604.
pub(super) fn destructive_command_refusal(
    command: &str,
    justification: Option<&str>,
    working_dir: Option<std::path::PathBuf>,
) -> Option<String> {
    let risk_ctx = jcode_command_risk::RiskContext::from_env(working_dir.clone());
    let assessment = jcode_command_risk::assess(command, &risk_ctx);
    if assessment.level.runs_immediately() {
        return None;
    }

    let justification = jcode_command_risk::Justification {
        text: justification.map(str::to_string),
    };
    let explanation = assessment.explanation();
    use crate::instruction::notification::Notification;
    let notification = match jcode_command_risk::gate(&assessment, &justification) {
        jcode_command_risk::GateOutcome::Allow => return None,
        jcode_command_risk::GateOutcome::Deny => {
            crate::logging::warn(&format!("[bash] denied destructive command: {command}"));
            Notification::DestructiveCommandDeny {
                explanation: &explanation,
            }
        }
        jcode_command_risk::GateOutcome::Reflect => {
            crate::logging::info(&format!(
                "[bash] destructive command held for justification: {command}"
            ));
            Notification::DestructiveCommandReflect {
                explanation: &explanation,
            }
        }
    };
    // Rendering cannot authorize execution. Even empty prose is still Some,
    // and a damaged instruction produces a visible refusal diagnostic.
    Some(
        notification
            .render(working_dir.as_deref())
            .unwrap_or_else(|error| {
                format!(
                    "Command was not run. Could not render command-refusal notification: {error}"
                )
            }),
    )
}

/// The `bash` tool's JSON schema, including the `justification` field the
/// destructive-command gate consumes.
///
/// Lives beside the gate so the schema and the policy that reads it stay in
/// sync, and so bash.rs stays inside the code-size budget.
pub(super) fn bash_parameters_schema() -> serde_json::Value {
    let cmd_desc = if cfg!(windows) {
        "The Windows command to execute via cmd.exe. Use cmd.exe syntax and quoting, not Bash syntax."
    } else {
        "The bash command to execute. Put large temp files under `$JCODE_SCRATCH_DIR`, not `/tmp`."
    };
    serde_json::json!({
        "type": "object",
        "required": ["command"],
        "properties": {
            "intent": crate::tool::intent_schema_property(),
            "command": {
                "type": "string",
                "description": cmd_desc
            },
            "timeout": {
                "type": "integer",
                "description": "Optional deadline in MILLISECONDS (not seconds), e.g. 600000 = 10min. Foreground commands continue as background tasks after the deadline; background commands are terminated with exit 124. Omit for no deadline."
            },
            "run_in_background": {
                "type": "boolean",
                "description": "Run in background. Emit `JCODE_PROGRESS {json}` lines for progress reporting."
            },
            "notify": {
                "type": "boolean",
                "description": "Notify on completion."
            },
            "wake": {
                "type": "boolean",
                "description": "Wake on completion."
            },
            "justification": {
                "type": "string",
                "description": "Only when re-issuing a command the destructive gate refused; explain which user request it serves."
            }
        }
    })
}
