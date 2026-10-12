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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activate_skill: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observe_startup_context: Option<bool>,
    /// Binds a server-rendered queued occurrence to its original client intent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_request_digest: Option<String>,
    /// What this input is for the session-work arrival rules. Producers set
    /// it; clients set it only for a human disposition other than queue.
    /// Absent, it is derived from `origin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrival: Option<jcode_session_work_types::ArrivalClass>,
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
            activate_skill: None,
            observe_startup_context: None,
            client_request_digest: None,
            arrival: None,
        }
    }

    /// The input's arrival class: its own, or the one its origin implies.
    /// Human-origin input and queued messages are Mirza's; anything else
    /// unclassified is an automatic notice.
    pub fn arrival_class(&self) -> jcode_session_work_types::ArrivalClass {
        use jcode_session_work_types::{ArrivalClass, HumanDisposition};
        if let Some(class) = &self.arrival {
            return class.clone();
        }
        match self.origin {
            Some(StoredMessageOrigin::Human) | Some(StoredMessageOrigin::Composed(_)) => {
                ArrivalClass::Human {
                    disposition: HumanDisposition::Queue,
                }
            }
            _ => ArrivalClass::SystemNotice,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredPrimaryInputReceipt {
    #[serde(default)]
    pub rolled_back: bool,
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

#[cfg(test)]
mod tests {
    use super::*;
    use jcode_session_work_types::{ArrivalClass, HumanDisposition};

    #[test]
    fn an_unclassified_input_keeps_its_old_wire_form_and_derives_its_class() {
        let mut input = PrimaryInputEnvelope::new(
            "session".into(),
            "hello".into(),
            PrimaryInputDelivery::SafeBoundary,
        );
        let json = serde_json::to_value(&input).unwrap();
        assert!(json.get("arrival").is_none(), "{json}");
        assert_eq!(input.arrival_class(), ArrivalClass::SystemNotice);
        input.origin = Some(StoredMessageOrigin::Human);
        assert_eq!(
            input.arrival_class(),
            ArrivalClass::Human {
                disposition: HumanDisposition::Queue
            }
        );
        input.origin = Some(StoredMessageOrigin::Composed(Vec::new()));
        assert!(input.arrival_class().is_human());
    }

    #[test]
    fn a_disposition_travels_on_the_envelope_and_unknown_fields_stay_rejected() {
        let mut input = PrimaryInputEnvelope::new(
            "session".into(),
            "hello".into(),
            PrimaryInputDelivery::NextTurn,
        );
        input.origin = Some(StoredMessageOrigin::Human);
        input.arrival = Some(ArrivalClass::Human {
            disposition: HumanDisposition::RevokeAndSend,
        });
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(
            json["arrival"],
            serde_json::json!({"class": "human", "disposition": "revoke_and_send"})
        );
        let back: PrimaryInputEnvelope = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back, input);
        assert_eq!(back.arrival_class(), input.arrival.clone().unwrap());
        let mut unknown = json;
        unknown["arrival_note"] = serde_json::json!("x");
        assert!(serde_json::from_value::<PrimaryInputEnvelope>(unknown).is_err());
    }
}
