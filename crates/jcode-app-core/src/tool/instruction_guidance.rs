//! Session-owned managed tool descriptions, frozen only after successful preflight.
use crate::{message::ToolDefinition, session::Session};

/// Read-only request preparation. Do not freeze a source that fails preflight.
pub fn preview(session: &Session, tools: &mut [ToolDefinition]) -> anyhow::Result<()> {
    if let Some(tool) = tools.iter_mut().find(|tool| tool.name == "subagent") {
        if let Some(text) = &session.delegation_guidance {
            tool.description = text.clone();
        } else {
            let body = crate::instruction::SystemPromptComposer::new().delegation_tool_guidance(
                session.working_dir.as_deref().map(std::path::Path::new),
            )?;
            if !body.is_empty() {
                tool.description = format!("{}\n\n{body}", tool.description);
            }
        }
    }
    if let Some(tool) = tools.iter_mut().find(|tool| tool.name == "workspace") {
        if let Some(text) = &session.workspace_guidance {
            tool.description = text.clone();
        } else {
            let body = crate::instruction::SystemPromptComposer::new().workspace_tool_guidance(
                session.working_dir.as_deref().map(std::path::Path::new),
            )?;
            if !body.is_empty() {
                tool.description = format!("{}\n\n{body}", tool.description);
            }
        }
    }
    if !crate::config::config().features.swarm {
        return Ok(());
    }
    let Some(tool) = tools.iter_mut().find(|tool| tool.name == "swarm") else {
        return Ok(());
    };
    if let Some(text) = &session.swarm_routing_prompt {
        tool.description = text.clone();
        return Ok(());
    }
    let (body, _) = crate::instruction::routing::swarm_routing_source(
        &crate::instruction::InstructionRepositoryService::new(),
        session.working_dir.as_deref().map(std::path::Path::new),
    )?;
    if !body.is_empty() {
        tool.description = format!(
            "{}\n\nSwarm prompt (user-tunable via /swarm-prompt):\n{body}",
            tool.description
        );
    }
    Ok(())
}

/// After successful request preflight, persist the exact description already
/// counted for this request. Returns whether an old session needs continuation reset.
///
/// `tools` is the request's provider array. A tool that joined through an
/// in-message tool-set notice is not in that array; the description the model
/// was given is the one in the session's tool-set record, so it is taken from
/// there.
pub fn commit(session: &mut Session, tools: &[ToolDefinition]) -> anyhow::Result<bool> {
    let offered: Vec<ToolDefinition> = session
        .tool_set
        .as_ref()
        .map(|record| record.effective())
        .unwrap_or_default();
    let find = |name: &str| {
        tools
            .iter()
            .find(|tool| tool.name == name)
            .or_else(|| offered.iter().find(|tool| tool.name == name))
            .cloned()
    };
    let swarm = (crate::config::config().features.swarm && session.swarm_routing_prompt.is_none())
        .then(|| find("swarm"))
        .flatten();
    let delegation = session
        .delegation_guidance
        .is_none()
        .then(|| find("subagent"))
        .flatten();
    let workspace = session
        .workspace_guidance
        .is_none()
        .then(|| find("workspace"))
        .flatten();
    if swarm.is_none() && delegation.is_none() && workspace.is_none() {
        return Ok(false);
    }
    let previous = session.clone();
    // Workspace guidance arrives with its tool, so its first capture is part of
    // the tool-set change already recorded for that request, not a migration
    // of a description the provider has already seen.
    let migrated = (swarm.is_some() || delegation.is_some())
        && (session.first_provider_dispatch_at().is_some()
            || session.provider_session_id.is_some());
    if let Some(tool) = swarm {
        session.set_swarm_routing_prompt(tool.description.clone());
    }
    if let Some(tool) = delegation {
        session.set_delegation_guidance(tool.description.clone());
    }
    if let Some(tool) = workspace {
        session.set_workspace_guidance(tool.description.clone());
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
        assert!(session.swarm_routing_prompt.is_none());
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
        assert!(fresh.swarm_routing_prompt.is_none());
        assert!(
            Session::load_startup_stub(&session.id)
                .unwrap()
                .swarm_routing_prompt
                .is_none()
        );
        assert_eq!(
            Session::load_for_remote_startup(&session.id)
                .unwrap()
                .swarm_routing_prompt,
            session.swarm_routing_prompt
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
        assert!(session.delegation_guidance.is_none());
        let expected = tools[0].description.clone();
        commit(&mut session, &tools).unwrap();
        std::fs::write(file, "invalid source").unwrap();
        let loaded = Session::load(&session.id).unwrap();
        tools[0].description = "BASE".into();
        preview(&loaded, &mut tools).unwrap();
        assert_eq!(tools[0].description, expected);
        let mut split = Session::create(Some(session.id.clone()), None);
        split.inherit_continuation_state_from(&loaded);
        assert_eq!(split.delegation_guidance, loaded.delegation_guidance);
        assert!(
            Session::load_startup_stub(&session.id)
                .unwrap()
                .delegation_guidance
                .is_none()
        );
        assert_eq!(
            Session::load_for_remote_startup(&session.id)
                .unwrap()
                .delegation_guidance,
            loaded.delegation_guidance
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
        assert!(session.workspace_guidance.is_none());
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
        assert_eq!(split.workspace_guidance, loaded.workspace_guidance);
        assert_eq!(
            Session::load_for_remote_startup(&session.id)
                .unwrap()
                .workspace_guidance,
            loaded.workspace_guidance
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
            });
        session.set_tool_set(record);
        let provider_array = vec![ToolDefinition {
            name: "bash".into(),
            description: "BASH".into(),
            input_schema: serde_json::json!({}),
        }];
        assert!(!commit(&mut session, &provider_array).unwrap());
        assert_eq!(
            session.workspace_guidance.as_deref(),
            Some("STRUCTURAL\n\nANNOUNCED TEXT")
        );
        assert_eq!(
            Session::load(&session.id).unwrap().workspace_guidance,
            session.workspace_guidance
        );
        // Later requests reuse the frozen text, whatever the store now says.
        let mut later = vec![structural("STRUCTURAL")];
        preview(&session, &mut later).unwrap();
        assert_eq!(later[0].description, "STRUCTURAL\n\nANNOUNCED TEXT");
    }
}
