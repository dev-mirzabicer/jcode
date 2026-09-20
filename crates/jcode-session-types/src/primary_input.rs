use crate::{StoredDisplayRole, StoredMessageOrigin, StoredUnattendedContextAuthorization};
use jcode_workspace_types::RequestId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimaryInputDelivery {
    SafeBoundary,
    NextTurn,
    ContextOnly,
}

/// Original accepted input. A receipt proves persistence, not provider dispatch
/// or semantic consumption. IDs are scoped to the owning primary Session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrimaryInputEnvelope {
    pub id: RequestId,
    pub session: String,
    pub delivery: PrimaryInputDelivery,
    pub content: String,
    #[serde(default)]
    pub images: Vec<(String, String)>,
    #[serde(default)]
    pub display_role: Option<StoredDisplayRole>,
    #[serde(default)]
    pub origin: Option<StoredMessageOrigin>,
    #[serde(default)]
    pub system_reminder: Option<String>,
    #[serde(default)]
    pub unattended_context: Option<StoredUnattendedContextAuthorization>,
    #[serde(default)]
    pub urgent: bool,
}

impl PrimaryInputEnvelope {
    pub fn new(session: String, content: String, delivery: PrimaryInputDelivery) -> Self {
        Self {
            id: RequestId::new(),
            session,
            delivery,
            content,
            images: Vec::new(),
            display_role: None,
            origin: None,
            system_reminder: None,
            unattended_context: None,
            urgent: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredPrimaryInputReceipt {
    pub id: RequestId,
    pub digest: String,
    pub messages: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimaryInputState {
    Accepted,
    Committed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrimaryInputReceipt {
    pub id: RequestId,
    pub session: String,
    pub state: PrimaryInputState,
    pub messages: Vec<String>,
    pub issue: Option<String>,
}
