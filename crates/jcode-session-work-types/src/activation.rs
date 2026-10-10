//! Session-work activation. A session is activated once, when it is created
//! with session work available; the fact never changes afterwards.
use super::*;

/// The kind of session an activation belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionWorkRole {
    /// A hosted primary session.
    Primary,
    /// An isolated child: workflow and completion only.
    Child,
}

/// How an activated session began.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActivationOrigin {
    /// A new session without inherited session work.
    Fresh {},
    /// Split from another session's conversation. No workflow is inherited.
    Split { source_session: String },
    /// Transferred from another session. Its workflow, if any, was copied.
    Transfer { source_session: String },
    /// An isolated child, with the task preset that created it.
    Child { preset: String },
}

/// One module type, frozen into a session at activation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleTypeDescriptor {
    /// Qualified selector, such as `global:research`.
    pub id: String,
    pub title: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtypes: Vec<String>,
    /// The skill that describes how to do this kind of module, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
}

/// The activation record a Session carries in its own persisted state. The
/// session-work store holds everything else; this proves the session expects
/// that store, so a missing store fails closed instead of looking empty.
/// Session snapshots stay readable across versions, so unknown fields here are
/// ignored like the rest of a Session's JSON.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionWorkBinding {
    pub role: SessionWorkRole,
    pub activated_at: chrono::DateTime<chrono::Utc>,
    /// The Session Context line rendered at activation, frozen with the
    /// session's other initial facts.
    pub context_line: String,
}

/// The store's activation row.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionWorkActivation {
    pub session: String,
    pub role: SessionWorkRole,
    pub origin: ActivationOrigin,
    pub activated_at: chrono::DateTime<chrono::Utc>,
    pub module_types: Vec<ModuleTypeDescriptor>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_records_reject_unknown_fields() {
        let activation = SessionWorkActivation {
            session: "session_fixture".into(),
            role: SessionWorkRole::Child,
            origin: ActivationOrigin::Child {
                preset: "global:task-preset.general".into(),
            },
            activated_at: chrono::Utc::now(),
            module_types: vec![ModuleTypeDescriptor {
                id: "global:research".into(),
                title: "Research".into(),
                description: "synthetic".into(),
                subtypes: vec!["web".into()],
                skill: None,
            }],
        };
        let json = serde_json::to_value(&activation).unwrap();
        assert_eq!(
            serde_json::from_value::<SessionWorkActivation>(json.clone()).unwrap(),
            activation
        );
        let mut extended = json;
        extended["future"] = serde_json::json!(true);
        assert!(serde_json::from_value::<SessionWorkActivation>(extended).is_err());
        assert!(serde_json::from_str::<ActivationOrigin>(r#"{"kind":"fresh","x":1}"#).is_err());
    }
}
