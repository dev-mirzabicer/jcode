//! Caller intent only. Provider objects, SQL state and runtime handles stay private.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize)]
#[serde(transparent)]
pub struct ChildSessionId(String);
impl ChildSessionId {
    pub fn parse(value: String) -> Result<Self, String> {
        if value.is_empty()
            || !value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            return Err(
                "Invalid child_id. A supplied child ID never creates a new conversation.".into(),
            );
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for ChildSessionId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    ReadOnly,
    ReadWrite,
}
impl Permission {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::ReadWrite => "read_write",
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "files", rename_all = "snake_case")]
pub enum ChildStartupContext {
    #[default]
    ProjectDefault,
    Disabled,
    Custom(Vec<String>),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateChild {
    pub agent: String,
    pub model_alias: String,
    pub permission: Permission,
    pub prompt: String,
    pub effort: Option<String>,
    pub preset: Option<String>,
    pub working_dir: Option<PathBuf>,
    pub startup_context: ChildStartupContext,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildMessage {
    pub child_id: ChildSessionId,
    pub prompt: String,
    pub permission: Option<Permission>,
    pub preset: Option<String>,
    pub queue_if_busy: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "request", rename_all = "snake_case")]
pub enum ChildRequest {
    Create(CreateChild),
    Send(ChildMessage),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChildDelivery {
    pub background: bool,
    pub notify: bool,
    pub wake: bool,
}
impl Default for ChildDelivery {
    fn default() -> Self {
        Self {
            background: false,
            notify: true,
            wake: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SubagentRequest {
    pub request: ChildRequest,
    pub delivery: ChildDelivery,
}

/// The model-facing flat schema has one interpretation. Null optional fields are
/// omission. Do not infer a creation from an empty or malformed child selector.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    child_id: Option<ChildSessionId>,
    agent: Option<String>,
    model_alias: Option<String>,
    permission: Option<Permission>,
    prompt: String,
    effort: Option<String>,
    preset: Option<String>,
    working_dir: Option<PathBuf>,
    startup_files: Option<Vec<String>>,
    disable_startup_context: Option<bool>,
    queue_if_busy: Option<bool>,
    run_in_background: Option<bool>,
    notify: Option<bool>,
    wake: Option<bool>,
    intent: Option<String>,
}

impl SubagentRequest {
    pub fn from_tool_input(value: serde_json::Value) -> Result<Self, String> {
        let input: Input = serde_json::from_value(value).map_err(|e| e.to_string())?;
        let _ = input.intent;
        if input.prompt.trim().is_empty() {
            return Err("prompt must be nonempty".into());
        }
        if input.preset.as_ref().is_some_and(|s| s.trim().is_empty()) {
            return Err("preset must be a nonempty selector".into());
        }
        let delivery = ChildDelivery {
            background: input.run_in_background.unwrap_or(false),
            notify: input.notify.unwrap_or(true),
            wake: input.wake.unwrap_or(false),
        };
        let request = if let Some(child_id) = input.child_id {
            if input.agent.is_some()
                || input.model_alias.is_some()
                || input.effort.is_some()
                || input.working_dir.is_some()
                || input.startup_files.is_some()
                || input.disable_startup_context.is_some()
            {
                return Err("Follow-ups cannot change profile, model, effort, working directory or initial Startup Context".into());
            }
            ChildRequest::Send(ChildMessage {
                child_id,
                prompt: input.prompt,
                permission: input.permission,
                preset: input.preset,
                queue_if_busy: input.queue_if_busy.unwrap_or(false),
            })
        } else {
            let agent = input
                .agent
                .filter(|s| !s.trim().is_empty())
                .ok_or("Creation requires an explicit agent profile")?;
            let model_alias = input
                .model_alias
                .filter(|s| !s.trim().is_empty())
                .ok_or("Creation requires an explicit model_alias")?;
            let permission = input
                .permission
                .ok_or("Creation requires explicit permission: read_only or read_write")?;
            if input.queue_if_busy.unwrap_or(false) {
                return Err(
                    "queue_if_busy applies only to follow-ups. Capacity overflow is never queued."
                        .into(),
                );
            }
            if input.disable_startup_context.unwrap_or(false) && input.startup_files.is_some() {
                return Err(
                    "disable_startup_context conflicts with startup_files, including an empty list"
                        .into(),
                );
            }
            let startup_context = if input.disable_startup_context.unwrap_or(false) {
                ChildStartupContext::Disabled
            } else if let Some(files) = input.startup_files {
                ChildStartupContext::Custom(files)
            } else {
                ChildStartupContext::ProjectDefault
            };
            ChildRequest::Create(CreateChild {
                agent,
                model_alias,
                permission,
                prompt: input.prompt,
                effort: input.effort,
                preset: input.preset,
                working_dir: input.working_dir,
                startup_context,
            })
        };
        Ok(Self { request, delivery })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn create() -> serde_json::Value {
        json!({"agent":"fixture","model_alias":"coder","permission":"read_only","prompt":"  exact task  "})
    }
    #[test]
    fn creation_requires_explicit_independent_choices() {
        for key in ["agent", "model_alias", "permission", "prompt"] {
            let mut value = create();
            value.as_object_mut().unwrap().remove(key);
            assert!(SubagentRequest::from_tool_input(value).is_err(), "{key}");
        }
        let request = SubagentRequest::from_tool_input(create()).unwrap();
        assert_eq!(request.delivery, ChildDelivery::default());
        let ChildRequest::Create(child) = request.request else {
            panic!()
        };
        assert_eq!(child.prompt, "  exact task  ");
        assert!(child.effort.is_none());
        assert_eq!(child.startup_context, ChildStartupContext::ProjectDefault);
    }
    #[test]
    fn followups_preserve_omission_and_reject_fixed_settings() {
        let value =
            json!({"child_id":"session_fixture", "prompt":"next", "permission":null,"preset":null});
        let request = SubagentRequest::from_tool_input(value.clone()).unwrap();
        let ChildRequest::Send(child) = request.request else {
            panic!()
        };
        assert!(child.permission.is_none() && child.preset.is_none() && !child.queue_if_busy);
        for (key, setting) in [
            ("agent", json!("x")),
            ("model_alias", json!("x")),
            ("effort", json!("none")),
            ("working_dir", json!(".")),
            ("startup_files", json!([])),
            ("disable_startup_context", json!(false)),
        ] {
            let mut changed = value.clone();
            changed[key] = setting;
            assert!(SubagentRequest::from_tool_input(changed).is_err(), "{key}");
        }
        let mut invalid = create();
        invalid["child_id"] = json!("");
        assert!(SubagentRequest::from_tool_input(invalid).is_err());
    }
    #[test]
    fn empty_custom_context_and_conflicting_disable_are_distinct() {
        let mut value = create();
        value["startup_files"] = json!([]);
        value["effort"] = serde_json::Value::Null;
        let request = SubagentRequest::from_tool_input(value.clone()).unwrap();
        let ChildRequest::Create(child) = request.request else {
            panic!()
        };
        assert_eq!(child.startup_context, ChildStartupContext::Custom(vec![]));
        assert!(child.effort.is_none());
        value["disable_startup_context"] = json!(true);
        assert!(SubagentRequest::from_tool_input(value).is_err());
    }
}
