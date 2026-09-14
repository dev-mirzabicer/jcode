//! Child identity and active controls belong to the authoritative conversation.
use super::*;
use crate::instruction::{InstructionKind, InstructionResourceRef, TaskPresetActivation};
use crate::model_roster::ModelRosterResolution;
use anyhow::{Result, ensure};
use jcode_tool_types::delegation::Permission;
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IsolatedChildIdentity {
    #[serde(default)]
    pub blocked_mcps: std::collections::BTreeSet<String>,
    pub profile: StoredAgentReference,
    pub original_parent: String,
    pub creation_run: String,
    pub working_dir: PathBuf,
    pub artifact_dir: PathBuf,
    pub resolution: ModelRosterResolution,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredIsolatedChild {
    pub identity: IsolatedChildIdentity,
    pub permission: Permission,
    pub preset: InstructionResourceRef,
    pub permission_message_id: String,
    pub preset_message_id: String,
    /// Structural index includes superseded controls but contains no prose.
    pub directive_message_ids: Vec<String>,
    pub last_started_run: Option<String>,
}

fn permission_text(child: &StoredIsolatedChild) -> Result<String> {
    Ok(format!(
        "<jcode_child_runtime>\n{}\n</jcode_child_runtime>",
        serde_json::to_string_pretty(&serde_json::json!({
            "parent_session": child.identity.original_parent,
            "working_directory": child.identity.working_dir,
            "artifact_directory": child.identity.artifact_dir,
            "permission": child.permission.as_str(),
        }))?
    ))
}

fn preset_open(preset: &InstructionResourceRef) -> String {
    format!(
        "<jcode_task_preset scope=\"{}\" id=\"{}\">",
        preset.scope, preset.id
    )
}

impl Session {
    /// Stages controls only. The caller validates complete context and persists
    /// this candidate before publication, never a half-installed live Session.
    pub fn install_isolated_child(
        &mut self,
        identity: IsolatedChildIdentity,
        permission: Permission,
        preset: TaskPresetActivation,
    ) -> Result<()> {
        ensure!(
            self.isolated_child.is_none() && self.parent_id.is_none(),
            "Child origin already exists or conflicts with primary ancestry"
        );
        ensure!(
            identity.original_parent != self.id && !identity.original_parent.is_empty(),
            "Invalid original parent"
        );
        ensure!(
            identity.working_dir.is_absolute() && identity.artifact_dir.is_absolute(),
            "Child paths must be resolved before installation"
        );
        ensure!(
            self.system_prompt.is_some(),
            "Child requires complete frozen instructions"
        );
        ensure!(
            self.active_agent() == Some(&identity.profile) && !self.is_canary,
            "Child profile differs from fixed identity"
        );
        ensure!(
            preset.resource.kind == InstructionKind::Notification
                && preset.resource.id.as_str().starts_with("task-preset."),
            "Invalid task preset resource"
        );
        self.model = Some(identity.resolution.selected_model().to_string());
        self.provider_key = Some(identity.resolution.provider_key());
        self.route_api_method = Some(identity.resolution.route_api_method().to_string());
        self.reasoning_effort = identity.resolution.selected_effort().map(str::to_string);
        self.working_dir = Some(
            identity
                .working_dir
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("Child working directory must be UTF-8"))?
                .to_string(),
        );
        let permission_id = new_id("message");
        let preset_id = new_id("message");
        let child = StoredIsolatedChild {
            identity,
            permission,
            preset: preset.resource,
            permission_message_id: permission_id.clone(),
            preset_message_id: preset_id.clone(),
            directive_message_ids: vec![permission_id.clone(), preset_id.clone()],
            last_started_run: None,
        };
        let runtime_text = permission_text(&child)?;
        let preset_text = format!(
            "{}\n{}\n</jcode_task_preset>",
            preset_open(&child.preset),
            preset.text
        );
        self.append_child_directive(permission_id, runtime_text);
        self.append_child_directive(preset_id, preset_text);
        self.isolated_child = Some(child);
        self.persist_state.force_snapshot = true;
        self.validate_isolated_child()
    }

    pub fn stage_child_settings(
        &mut self,
        permission: Option<Permission>,
        preset: Option<TaskPresetActivation>,
    ) -> Result<()> {
        self.validate_isolated_child()?;
        let mut child = self
            .isolated_child
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Not an isolated child"))?;
        if let Some(permission) = permission
            && permission != child.permission
        {
            child.permission = permission;
            let id = new_id("message");
            child.permission_message_id = id.clone();
            child.directive_message_ids.push(id.clone());
            self.append_child_directive(id, permission_text(&child)?);
        }
        if let Some(preset) = preset
            && preset.resource != child.preset
        {
            ensure!(
                preset.resource.kind == InstructionKind::Notification
                    && preset.resource.id.as_str().starts_with("task-preset."),
                "Invalid task preset resource"
            );
            child.preset = preset.resource;
            let id = new_id("message");
            child.preset_message_id = id.clone();
            child.directive_message_ids.push(id.clone());
            self.append_child_directive(
                id,
                format!(
                    "{}\n{}\n</jcode_task_preset>",
                    preset_open(&child.preset),
                    preset.text
                ),
            );
        }
        self.isolated_child = Some(child);
        self.persist_state.force_snapshot = true;
        self.validate_isolated_child()
    }

    fn append_child_directive(&mut self, id: String, text: String) {
        self.append_stored_message(StoredMessage {
            id,
            role: Role::User,
            content: vec![ContentBlock::Text {
                text,
                cache_control: None,
            }],
            display_role: Some(StoredDisplayRole::System),
            timestamp: None,
            origin: None,
            tool_duration_ms: None,
            token_usage: None,
        });
    }

    /// Current directives are protected by the existing context owner. Old
    /// superseded notices remain ordinary history, not a second context view.
    pub fn active_child_directive_ids(&self) -> Vec<String> {
        self.isolated_child
            .as_ref()
            .map(|child| {
                vec![
                    child.permission_message_id.clone(),
                    child.preset_message_id.clone(),
                ]
            })
            .unwrap_or_default()
    }

    pub fn validate_isolated_child(&self) -> Result<()> {
        let Some(child) = &self.isolated_child else {
            return Ok(());
        };
        let identity = &child.identity;
        ensure!(
            self.parent_id.is_none()
                && identity.original_parent != self.id
                && !identity.original_parent.is_empty(),
            "Invalid isolated child origin"
        );
        ensure!(
            self.system_prompt.is_some() && !self.is_canary,
            "Child frozen system is missing or selfdev policy was adopted"
        );
        ensure!(
            self.active_agent() == Some(&identity.profile),
            "Child profile differs from fixed identity"
        );
        ensure!(
            self.model.as_deref() == Some(identity.resolution.selected_model())
                && self.provider_key.as_deref()
                    == Some(identity.resolution.provider_key().as_str())
                && self.route_api_method.as_deref() == Some(identity.resolution.route_api_method())
                && self.reasoning_effort.as_deref() == identity.resolution.selected_effort()
                && self.working_dir.as_deref().map(std::path::Path::new)
                    == Some(identity.working_dir.as_path()),
            "Child model, effort or working directory differs from its fixed execution identity"
        );
        ensure!(
            identity.artifact_dir.is_absolute(),
            "Child artifact identity is invalid"
        );
        for id in [&child.permission_message_id, &child.preset_message_id] {
            ensure!(
                child
                    .directive_message_ids
                    .iter()
                    .filter(|entry| *entry == id)
                    .count()
                    == 1,
                "Active child directive is not uniquely registered"
            );
            let mut messages = self.messages.iter().filter(|message| &message.id == id);
            let message = messages
                .next()
                .ok_or_else(|| anyhow::anyhow!("Active child directive {id} is missing"))?;
            ensure!(
                messages.next().is_none(),
                "Active child directive is duplicated"
            );
            ensure!(
                message.role == Role::User
                    && message.display_role == Some(StoredDisplayRole::System)
                    && message.timestamp.is_none(),
                "Invalid child directive authority"
            );
            let [ContentBlock::Text { text, .. }] = message.content.as_slice() else {
                anyhow::bail!("Invalid child directive content");
            };
            if id == &child.permission_message_id {
                ensure!(
                    text == &permission_text(child)?,
                    "Child permission notice differs from active authority"
                );
            } else {
                ensure!(
                    text.starts_with(&format!("{}\n", preset_open(&child.preset)))
                        && text.ends_with("\n</jcode_task_preset>"),
                    "Child preset notice differs from active identity"
                );
            }
        }
        ensure!(
            child.permission_message_id != child.preset_message_id,
            "Child controls share a message identity"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instruction::{InstructionId, InstructionScope};

    fn preset(id: &str, text: &str) -> TaskPresetActivation {
        TaskPresetActivation {
            resource: InstructionResourceRef {
                kind: InstructionKind::Notification,
                scope: InstructionScope::Global,
                id: InstructionId::parse(format!("task-preset.{id}")).unwrap(),
            },
            text: text.into(),
        }
    }

    fn child() -> Session {
        let mut session = Session::create_with_id("session_child_fixture".into(), None, None);
        let profile = StoredAgentReference {
            scope: InstructionScope::Global,
            id: "fixture".into(),
            display_name: "Fixture".into(),
        };
        session.install_system_prompt(StoredSystemPromptState {
            text: "FROZEN SYSTEM".into(),
            active_agent: profile.clone(),
            first_provider_dispatch_at: None,
            active_transition_message_id: None,
        });
        let selection = jcode_provider_core::RouteSelection {
            model: "synthetic-model".into(),
            runtime_key: jcode_provider_core::RuntimeKey::OpenAIOAuth,
            api_method: "openai-oauth".into(),
            provider_label: "OpenAI".into(),
            detail: String::new(),
        };
        let resolution = serde_json::from_value(serde_json::json!({
            "requested_alias":"fixture", "selection": selection, "selected_effort":"high",
            "used_model_override":false,"used_effort_override":false,
        }))
        .unwrap();
        session
            .install_isolated_child(
                IsolatedChildIdentity {
                    blocked_mcps: Default::default(),
                    profile,
                    original_parent: "session_parent_fixture".into(),
                    creation_run: "run-fixture".into(),
                    working_dir: std::env::current_dir().unwrap(),
                    artifact_dir: std::env::current_dir().unwrap().join("fixture-artifacts"),
                    resolution,
                },
                Permission::ReadOnly,
                preset("general", "INITIAL"),
            )
            .unwrap();
        session
    }

    #[test]
    fn child_roundtrips_full_metadata_and_readonly_capture_without_source_access() {
        let home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
        let mut session = child();
        session.save().unwrap();
        session
            .stage_child_settings(Some(Permission::ReadWrite), Some(preset("other", "SECOND")))
            .unwrap();
        session.save().unwrap();
        let loaded = Session::load(&session.id).unwrap();
        assert_eq!(loaded.isolated_child, session.isolated_child);
        loaded.validate_active_agent_profile().unwrap();
        let capture = Session::capture_readonly(home.root(), &session.id).unwrap();
        assert_eq!(capture.session().isolated_child, session.isolated_child);
        let stub = Session::load_startup_stub(&session.id).unwrap();
        assert_eq!(stub.isolated_child, session.isolated_child);
        assert!(stub.system_prompt.is_none() && stub.messages.is_empty());
        let remote = Session::load_for_remote_startup(&session.id).unwrap();
        assert_eq!(remote.isolated_child, session.isolated_child);
        assert_eq!(
            Session::inspection_relationships(home.root(), &session.id, None).unwrap(),
            (None, Some("session_parent_fixture".into()))
        );
    }

    #[test]
    fn child_settings_append_and_only_current_directives_remain_active() {
        let mut session = child();
        let original = session.messages.clone();
        let system = session.system_prompt.clone();
        let old_ids = session.active_child_directive_ids();
        session.stage_child_settings(None, None).unwrap();
        assert_eq!(
            serde_json::to_value(&session.messages).unwrap(),
            serde_json::to_value(&original).unwrap()
        );
        session
            .stage_child_settings(
                Some(Permission::ReadWrite),
                Some(preset("special", "SPECIAL")),
            )
            .unwrap();
        assert_eq!(session.messages.len(), original.len() + 2);
        assert_eq!(
            serde_json::to_value(&session.messages[..original.len()]).unwrap(),
            serde_json::to_value(&original).unwrap()
        );
        assert!(
            session
                .active_child_directive_ids()
                .iter()
                .all(|id| !old_ids.contains(id))
        );
        assert_eq!(session.system_prompt, system);
        session
            .stage_child_settings(None, Some(preset("general", "")))
            .unwrap();
        session.validate_active_agent_profile().unwrap();
        assert_eq!(
            session.isolated_child.as_ref().unwrap().permission,
            Permission::ReadWrite
        );
    }

    #[test]
    fn child_corruption_fails_without_reinterpreting_fixed_settings() {
        let session = child();
        for index in 0..2 {
            let mut missing = session.clone();
            missing.messages.remove(index);
            assert!(missing.validate_active_agent_profile().is_err());
            let mut duplicate = session.clone();
            duplicate.messages.push(session.messages[index].clone());
            assert!(duplicate.validate_active_agent_profile().is_err());
        }
        let mut changed = session.clone();
        changed.reasoning_effort = Some("low".into());
        assert!(changed.validate_active_agent_profile().is_err());
        let mut changed = session.clone();
        changed.isolated_child.as_mut().unwrap().permission = Permission::ReadWrite;
        assert!(changed.validate_active_agent_profile().is_err());
        let mut split = Session::create_with_id(
            "session_split_fixture".into(),
            Some(session.id.clone()),
            None,
        );
        split.inherit_continuation_state_from(&session);
        assert!(split.isolated_child.is_none());
    }
}
