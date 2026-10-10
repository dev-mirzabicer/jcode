//! Session-owned managed tool text, frozen only after successful preflight.
//!
//! A tool's managed text is its description (delegation, workspace, Swarm
//! routing and `declare_stop` guidance) and, in sessions with session work,
//! the wording of some background-work parameters. Each session freezes what
//! its first successful request carried in one map keyed by tool name; the
//! older single-tool Session fields are read as entries of that map.
use crate::session::StoredToolGuidance;
use crate::{message::ToolDefinition, session::Session};
use std::collections::BTreeMap;

/// Tools that may carry managed text, in a fixed order.
const MANAGED_TOOLS: [&str; 6] = [
    "subagent",
    "workspace",
    "swarm",
    "declare_stop",
    "bash",
    "bg",
];

/// Read-only request preparation. Do not freeze a source that fails preflight.
pub fn preview(session: &Session, tools: &mut [ToolDefinition]) -> anyhow::Result<()> {
    for tool in tools.iter_mut() {
        if !MANAGED_TOOLS.contains(&tool.name.as_str()) {
            continue;
        }
        if tool.name == "swarm" && !crate::config::config().features.swarm {
            continue;
        }
        let guidance = match session.frozen_tool_guidance(&tool.name) {
            Some(frozen) => frozen,
            None => live_guidance(session, tool)?,
        };
        apply(tool, &guidance);
    }
    Ok(())
}

/// The managed text the current sources give `tool` in this session.
fn live_guidance(session: &Session, tool: &ToolDefinition) -> anyhow::Result<StoredToolGuidance> {
    let working_dir = session.working_dir.as_deref().map(std::path::Path::new);
    let composer = crate::instruction::SystemPromptComposer::new();
    let appended = |body: String| {
        (!body.is_empty()).then(|| {
            if tool.description.is_empty() {
                body
            } else {
                format!("{}\n\n{body}", tool.description)
            }
        })
    };
    let description = match tool.name.as_str() {
        "subagent" => appended(composer.delegation_tool_guidance(working_dir)?),
        "workspace" => appended(composer.workspace_tool_guidance(working_dir)?),
        "swarm" => {
            let (body, _) = crate::instruction::routing::swarm_routing_source(
                &crate::instruction::InstructionRepositoryService::new(),
                working_dir,
            )?;
            (!body.is_empty()).then(|| {
                format!(
                    "{}\n\nSwarm prompt (user-tunable via /swarm-prompt):\n{body}",
                    tool.description
                )
            })
        }
        "declare_stop" => match &session.session_work {
            Some(binding) => {
                appended(composer.declare_stop_tool_guidance(binding.role, working_dir)?)
            }
            None => None,
        },
        _ => None,
    };
    let parameters = match &session.session_work {
        Some(_) => session_work_parameters(tool, working_dir)?,
        None => BTreeMap::new(),
    };
    Ok(StoredToolGuidance {
        description,
        parameters,
    })
}

/// New-session wording for background-work parameters.
fn session_work_parameters(
    tool: &ToolDefinition,
    working_dir: Option<&std::path::Path>,
) -> anyhow::Result<BTreeMap<String, String>> {
    use crate::instruction::notification::Notification;
    let render = |notification: Notification<'_>| notification.render(working_dir);
    let structural = |name: &str| {
        tool.input_schema
            .pointer(&format!("/properties/{name}/description"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let mut parameters = BTreeMap::new();
    match tool.name.as_str() {
        "bash" => {
            parameters.insert(
                "wake".into(),
                render(Notification::SessionWorkBackgroundWake)?,
            );
            parameters.insert(
                "timeout".into(),
                format!(
                    "{} {}",
                    crate::tool::bash::TIMEOUT_UNITS_DESCRIPTION,
                    render(Notification::SessionWorkBashTimeout)?
                ),
            );
        }
        "bg" => {
            parameters.insert(
                "wake".into(),
                render(Notification::SessionWorkBackgroundWake)?,
            );
            parameters.insert(
                "max_wait_seconds".into(),
                render(Notification::SessionWorkBgMaxWait)?,
            );
            parameters.insert(
                "task_id".into(),
                format!(
                    "{} {}",
                    structural("task_id"),
                    render(Notification::SessionWorkBgTaskAlias)?
                ),
            );
        }
        "subagent" => {
            parameters.insert(
                "wake".into(),
                render(Notification::SessionWorkBackgroundWake)?,
            );
        }
        _ => {}
    }
    parameters.retain(|name, _| {
        tool.input_schema
            .pointer(&format!("/properties/{name}"))
            .is_some()
    });
    Ok(parameters)
}

fn apply(tool: &mut ToolDefinition, guidance: &StoredToolGuidance) {
    if let Some(description) = &guidance.description {
        tool.description = description.clone();
    }
    for (name, text) in &guidance.parameters {
        if let Some(property) = tool
            .input_schema
            .pointer_mut(&format!("/properties/{name}"))
            .and_then(serde_json::Value::as_object_mut)
        {
            property.insert(
                "description".into(),
                serde_json::Value::String(text.clone()),
            );
        }
    }
}

/// The managed text a sent definition carries, given what is managed for it.
fn captured(definition: &ToolDefinition, live: &StoredToolGuidance) -> StoredToolGuidance {
    StoredToolGuidance {
        description: live
            .description
            .as_ref()
            .map(|_| definition.description.clone()),
        parameters: live
            .parameters
            .keys()
            .filter_map(|name| {
                definition
                    .input_schema
                    .pointer(&format!("/properties/{name}/description"))
                    .and_then(serde_json::Value::as_str)
                    .map(|text| (name.clone(), text.to_string()))
            })
            .collect(),
    }
}

/// After successful request preflight, persist the exact managed text already
/// counted for this request. Returns whether an old session needs continuation
/// reset.
///
/// `tools` is the request's provider array. A tool that joined through an
/// in-message tool-set notice is not in that array; the text the model was
/// given is the one in the session's tool-set record, so it is taken from
/// there.
pub fn commit(session: &mut Session, tools: &[ToolDefinition]) -> anyhow::Result<bool> {
    let offered: Vec<ToolDefinition> = session
        .tool_set
        .as_ref()
        .map(|record| record.effective())
        .unwrap_or_default();
    let mut captures = Vec::new();
    for name in MANAGED_TOOLS {
        if name == "swarm" && !crate::config::config().features.swarm {
            continue;
        }
        if session.frozen_tool_guidance(name).is_some() {
            continue;
        }
        let Some(definition) = tools
            .iter()
            .find(|tool| tool.name == name)
            .or_else(|| offered.iter().find(|tool| tool.name == name))
        else {
            continue;
        };
        // What is managed for this tool comes from the session's own rules;
        // the text frozen is the text the request carried.
        let live = live_guidance_shape(session, definition);
        if live.description.is_none() && live.parameters.is_empty() {
            continue;
        }
        captures.push((name, captured(definition, &live)));
    }
    if captures.is_empty() {
        return Ok(false);
    }
    let previous = session.clone();
    // Delegation and Swarm guidance used to be captured late; a session that
    // already sent requests gets a continuation reset. Other managed text
    // arrives with its tool, as part of the tool-set change already recorded.
    let migrated = captures
        .iter()
        .any(|(name, _)| matches!(*name, "swarm" | "subagent"))
        && (session.first_provider_dispatch_at().is_some()
            || session.provider_session_id.is_some());
    for (name, guidance) in captures {
        session.set_tool_guidance(name, guidance);
    }
    if migrated {
        session.provider_session_id = None;
    }
    if let Err(error) = session.save() {
        *session = previous;
        return Err(error);
    }
    Ok(migrated)
}

/// Which parts of a tool are managed in this session, without reading
/// sources: descriptions of the guided tools, and in sessions with session
/// work the background-work parameters.
fn live_guidance_shape(session: &Session, definition: &ToolDefinition) -> StoredToolGuidance {
    let description = match definition.name.as_str() {
        "subagent" | "workspace" | "swarm" => Some(String::new()),
        "declare_stop" => session.session_work.as_ref().map(|_| String::new()),
        _ => None,
    };
    let names: &[&str] = match (definition.name.as_str(), &session.session_work) {
        ("bash", Some(_)) => &["wake", "timeout"],
        ("bg", Some(_)) => &["wake", "max_wait_seconds", "task_id"],
        ("subagent", Some(_)) => &["wake"],
        _ => &[],
    };
    StoredToolGuidance {
        description,
        parameters: names
            .iter()
            .filter(|name| {
                definition
                    .input_schema
                    .pointer(&format!("/properties/{name}"))
                    .is_some()
            })
            .map(|name| (name.to_string(), String::new()))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tools() -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            name: "swarm".into(),
            description: "BASE".into(),
            input_schema: serde_json::json!({"type":"object"}),
        }]
    }
    fn write(home: &std::path::Path, body: &str) {
        std::fs::write(
            home.join("instructions/tools/swarm-routing.md"),
            format!(
                "---\nid: swarm-routing\nkind: tool-guidance\ntemplate: handlebars\n---\n{body}"
            ),
        )
        .unwrap();
    }
    #[test]
    fn routing_is_frozen_only_after_preflight_and_survives_snapshot_journal_resume_and_split() {
        let home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
        crate::instruction::SystemPromptComposer::new()
            .ensure_global_store()
            .unwrap();
        let mut session = Session::create(None, None);
        session.save().unwrap();
        write(home.root(), "FIRST");
        let mut first = tools();
        preview(&session, &mut first).unwrap();
        assert!(session.frozen_tool_guidance("swarm").is_none());
        write(home.root(), "SECOND");
        let mut second = tools();
        preview(&session, &mut second).unwrap();
        assert!(first[0].description.ends_with("FIRST"));
        assert!(second[0].description.ends_with("SECOND"));
        assert!(!commit(&mut session, &second).unwrap());
        session.add_message(
            crate::message::Role::User,
            vec![crate::message::ContentBlock::Text {
                text: "APPEND".into(),
                cache_control: None,
            }],
        );
        session.save().unwrap();
        write(home.root(), "{{missing}}");
        let resumed = Session::load(&session.id).unwrap();
        let mut restored = tools();
        preview(&resumed, &mut restored).unwrap();
        assert_eq!(
            serde_json::to_value(&restored).unwrap(),
            serde_json::to_value(&second).unwrap()
        );
        let mut split = Session::create(Some(session.id.clone()), None);
        split.inherit_continuation_state_from(&resumed);
        split.save().unwrap();
        let split = Session::load(&split.id).unwrap();
        let mut restored = tools();
        preview(&split, &mut restored).unwrap();
        assert_eq!(
            serde_json::to_value(&restored).unwrap(),
            serde_json::to_value(&second).unwrap()
        );
        let mut fresh = Session::create(None, None);
        let before = serde_json::to_value(&fresh).unwrap();
        assert!(preview(&fresh, &mut tools()).is_err());
        assert_eq!(serde_json::to_value(&fresh).unwrap(), before);
        preview(&fresh, &mut []).unwrap();
        assert!(!commit(&mut fresh, &[]).unwrap());
        assert!(fresh.frozen_tool_guidance("swarm").is_none());
        assert!(
            Session::load_startup_stub(&session.id)
                .unwrap()
                .frozen_tool_guidance("swarm")
                .is_none()
        );
        assert_eq!(
            Session::load_for_remote_startup(&session.id)
                .unwrap()
                .frozen_tool_guidance("swarm"),
            session.frozen_tool_guidance("swarm")
        );
        write(home.root(), "");
        let mut empty = tools();
        preview(&fresh, &mut empty).unwrap();
        assert_eq!(empty[0].description, "BASE");
        fresh.provider_session_id = Some("OLD-CONTINUATION".into());
        assert!(commit(&mut fresh, &empty).unwrap());
        assert!(fresh.provider_session_id.is_none());
    }
}

#[cfg(test)]
mod failure_tests {
    use super::*;
    #[test]
    fn routing_snapshot_save_failure_restores_session_and_continuation() {
        let home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
        let mut session = Session::create(None, None);
        session.provider_session_id = Some("KEEP".into());
        session.save().unwrap();
        let before = serde_json::to_value(&session).unwrap();
        let blocker = home.root().join("not-a-directory");
        std::fs::write(&blocker, "fixture").unwrap();
        crate::env::set_var("JCODE_HOME", &blocker);
        let tools = vec![ToolDefinition {
            name: "swarm".into(),
            description: "SYNTHETIC VALIDATED DESCRIPTION".into(),
            input_schema: serde_json::json!({}),
        }];
        assert!(commit(&mut session, &tools).is_err());
        assert_eq!(serde_json::to_value(&session).unwrap(), before);
    }
}

#[cfg(test)]
mod delegation_tests {
    use super::*;
    #[test]
    fn delegation_guidance_freezes_only_after_preflight_and_survives_source_edits() {
        let home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
        crate::instruction::SystemPromptComposer::new()
            .ensure_global_store()
            .unwrap();
        let file = home.root().join("instructions/tools/subagent.md");
        std::fs::write(
            &file,
            "---\nid: subagent\nkind: tool-guidance\n---\nSYNTHETIC FIRST",
        )
        .unwrap();
        let mut session = Session::create(None, None);
        let mut tools = vec![ToolDefinition {
            name: "subagent".into(),
            description: "STRUCTURAL TOOL".into(),
            input_schema: serde_json::json!({}),
        }];
        preview(&session, &mut tools).unwrap();
        assert!(session.frozen_tool_guidance("subagent").is_none());
        let expected = tools[0].description.clone();
        commit(&mut session, &tools).unwrap();
        std::fs::write(file, "invalid source").unwrap();
        let loaded = Session::load(&session.id).unwrap();
        tools[0].description = "BASE".into();
        preview(&loaded, &mut tools).unwrap();
        assert_eq!(tools[0].description, expected);
        let mut split = Session::create(Some(session.id.clone()), None);
        split.inherit_continuation_state_from(&loaded);
        assert_eq!(
            split.frozen_tool_guidance("subagent"),
            loaded.frozen_tool_guidance("subagent")
        );
        assert!(
            Session::load_startup_stub(&session.id)
                .unwrap()
                .frozen_tool_guidance("subagent")
                .is_none()
        );
        assert_eq!(
            Session::load_for_remote_startup(&session.id)
                .unwrap()
                .frozen_tool_guidance("subagent"),
            loaded.frozen_tool_guidance("subagent")
        );
    }
}

#[cfg(test)]
mod workspace_tests {
    use super::*;
    #[test]
    fn workspace_guidance_freezes_with_its_tool_without_resetting_continuation() {
        let home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
        crate::instruction::SystemPromptComposer::new()
            .ensure_global_store()
            .unwrap();
        let file = home.root().join("instructions/tools/workspace.md");
        // The seeded resource is the managed source; edits replace it.
        assert!(file.is_file());
        std::fs::write(
            &file,
            "---\nid: workspace\nkind: tool-guidance\n---\nSYNTHETIC WORKSPACE FIRST",
        )
        .unwrap();
        let mut session = Session::create(None, None);
        session.provider_session_id = Some("KEEP-CONTINUATION".into());
        let mut tools = vec![ToolDefinition {
            name: "workspace".into(),
            description: "STRUCTURAL TOOL".into(),
            input_schema: serde_json::json!({}),
        }];
        preview(&session, &mut tools).unwrap();
        assert!(session.frozen_tool_guidance("workspace").is_none());
        assert!(tools[0].description.starts_with("STRUCTURAL TOOL\n\n"));
        assert!(tools[0].description.ends_with("SYNTHETIC WORKSPACE FIRST"));
        let expected = tools[0].description.clone();
        // Arriving with its tool is part of that recorded tool-set change.
        assert!(!commit(&mut session, &tools).unwrap());
        assert_eq!(
            session.provider_session_id.as_deref(),
            Some("KEEP-CONTINUATION")
        );
        std::fs::write(&file, "invalid source").unwrap();
        let loaded = Session::load(&session.id).unwrap();
        tools[0].description = "STRUCTURAL TOOL".into();
        preview(&loaded, &mut tools).unwrap();
        assert_eq!(tools[0].description, expected);
        let mut split = Session::create(Some(session.id.clone()), None);
        split.inherit_continuation_state_from(&loaded);
        assert_eq!(
            split.frozen_tool_guidance("workspace"),
            loaded.frozen_tool_guidance("workspace")
        );
        assert_eq!(
            Session::load_for_remote_startup(&session.id)
                .unwrap()
                .frozen_tool_guidance("workspace"),
            loaded.frozen_tool_guidance("workspace")
        );
    }

    #[test]
    fn workspace_guidance_announced_inside_a_message_freezes_from_the_tool_set_record() {
        let _home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
        let structural = |description: &str| ToolDefinition {
            name: "workspace".into(),
            description: description.into(),
            input_schema: serde_json::json!({}),
        };
        let mut session = Session::create(None, None);
        // Providers that take tool changes inside a message keep their first
        // `tools` array; the workspace tool arrives in a persisted notice.
        let mut record = jcode_session_types::StoredToolSet::new(vec![ToolDefinition {
            name: "bash".into(),
            description: "BASH".into(),
            input_schema: serde_json::json!({}),
        }]);
        record
            .changes
            .push(jcode_session_types::StoredToolSetChange {
                change: crate::message::ToolSetChange::Added {
                    definition: structural("STRUCTURAL\n\nANNOUNCED TEXT"),
                },
                schema_changed: false,
                withdrawn: false,
            });
        session.set_tool_set(record);
        let provider_array = vec![ToolDefinition {
            name: "bash".into(),
            description: "BASH".into(),
            input_schema: serde_json::json!({}),
        }];
        assert!(!commit(&mut session, &provider_array).unwrap());
        assert_eq!(
            session
                .frozen_tool_guidance("workspace")
                .and_then(|guidance| guidance.description)
                .as_deref(),
            Some("STRUCTURAL\n\nANNOUNCED TEXT")
        );
        assert_eq!(
            Session::load(&session.id)
                .unwrap()
                .frozen_tool_guidance("workspace"),
            session.frozen_tool_guidance("workspace")
        );
        // Later requests reuse the frozen text, whatever the store now says.
        let mut later = vec![structural("STRUCTURAL")];
        preview(&session, &mut later).unwrap();
        assert_eq!(later[0].description, "STRUCTURAL\n\nANNOUNCED TEXT");
    }
}
