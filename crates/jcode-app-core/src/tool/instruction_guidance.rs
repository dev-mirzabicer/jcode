//! One Swarm description snapshot, owned by the ordinary session lifecycle.
use crate::{message::ToolDefinition, session::Session};

/// Read-only request preparation. Do not freeze a source that fails preflight.
pub fn preview(session: &Session, tools: &mut [ToolDefinition]) -> anyhow::Result<()> {
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
pub fn commit(session: &mut Session, tools: &[ToolDefinition]) -> anyhow::Result<bool> {
    if session.swarm_routing_prompt.is_some() {
        return Ok(false);
    }
    let Some(tool) = tools.iter().find(|tool| tool.name == "swarm") else {
        return Ok(false);
    };
    let previous = session.clone();
    let migrated =
        session.first_provider_dispatch_at().is_some() || session.provider_session_id.is_some();
    session.set_swarm_routing_prompt(tool.description.clone());
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
