//! Arrivals: input that reaches a session from outside its own turn, such as
//! Mirza's messages, finished background work, answers, timers and
//! scheduled resumes. One rule decides whether each arrival is delivered,
//! held or ends the session's stop; these types describe that decision.
use super::*;

/// How Mirza wants a message handled when it arrives during a declared stop.
/// Each client presents the choice its own way.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanDisposition {
    /// Hold the message until the session wakes.
    #[default]
    Queue,
    /// Deliver it now; the stop stays in place.
    SendKeepStop,
    /// Withdraw the stop and deliver it.
    RevokeAndSend,
}

impl HumanDisposition {
    pub fn is_queue(&self) -> bool {
        *self == Self::Queue
    }
}

/// The item an arrival belongs to, named by the request that allocates its
/// alias. The kind, label and policy allocate the item if it was never seen
/// while running; an item already allocated keeps its own.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemSource {
    pub request: String,
    pub kind: ItemKind,
    pub label: String,
    pub policy: FinishPolicy,
}

/// What an input is, for the arrival rules. Producers classify their own
/// input; a missing class is derived from the input's origin.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case", deny_unknown_fields)]
pub enum ArrivalClass {
    /// A message from Mirza through a trusted client.
    Human {
        #[serde(default, skip_serializing_if = "HumanDisposition::is_queue")]
        disposition: HumanDisposition,
    },
    /// Background work or a sub-agent run reached a final state.
    ItemFinished { item: ItemSource },
    /// Mirza answered a question, submitted an artifact or decided a
    /// session proposal. Counts as human input for crash recovery.
    Answer { item: ItemSource },
    /// The time limit of stop `stop` passed.
    TimerExpired { stop: u32 },
    /// A scheduled resume fired.
    ScheduledResume {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item: Option<ItemSource>,
    },
    /// A reload or recovery continues an interrupted turn.
    Continuation,
    /// Any other automatic notice.
    SystemNotice,
}

impl ArrivalClass {
    /// Input that carries Mirza's own authority: it proceeds past an
    /// unresolved crash recovery, where automatic input waits.
    pub fn is_human(&self) -> bool {
        matches!(self, Self::Human { .. } | Self::Answer { .. })
    }

    /// The item this arrival finishes, if any.
    pub fn item(&self) -> Option<&ItemSource> {
        match self {
            Self::ItemFinished { item } | Self::Answer { item } => Some(item),
            Self::ScheduledResume { item } => item.as_ref(),
            _ => None,
        }
    }
}

/// What the session was doing when an arrival was held.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "while", rename_all = "snake_case", deny_unknown_fields)]
pub enum HeldWhile {
    /// A wait was declared on this condition.
    Waiting { condition: WaitCondition },
    /// The session had stopped: completed, interrupted or failed, or a
    /// completion was being declared.
    Stopped,
}

/// The arrival rule's decision for one input, made once when the input
/// first arrives.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "route", rename_all = "snake_case", deny_unknown_fields)]
pub enum ArrivalRoute {
    /// Deliver at the next safe point, or start a turn when idle.
    Deliver,
    /// Keep it until the session wakes.
    Held { held_while: HeldWhile },
    /// It met the wait condition of stop `stop`, which it withdrew.
    EndsStop { stop: u32 },
    /// Mirza sent it during stop `stop`, which stays in place.
    KeepsStop { stop: u32 },
    /// Mirza sent it and withdrew stop `stop`.
    RevokesStop { stop: u32 },
    /// The time limit of stop `stop` passed; the stop is withdrawn.
    TimesOut { stop: u32 },
    /// It refers to a stop that no longer governs the session and carries
    /// nothing to deliver.
    Obsolete,
}

/// One arrival as the session-work store records it. `input` is the
/// durable primary input's identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArrivalRecord {
    pub input: String,
    pub class: ArrivalClass,
    pub arrived_at: chrono::DateTime<chrono::Utc>,
    pub route: ArrivalRoute,
    /// The item it finished, with its alias and label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<SessionItem>,
    /// When a held arrival was released for delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl ArrivalRecord {
    /// Held and not yet released.
    pub fn is_held(&self) -> bool {
        matches!(self.route, ArrivalRoute::Held { .. }) && self.released_at.is_none()
    }
}

/// The arrival contract version advertised to clients that set a class or a
/// human disposition on primary input.
pub const ARRIVAL_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_disposition_is_omitted_and_classes_round_trip() {
        let human = ArrivalClass::Human {
            disposition: HumanDisposition::Queue,
        };
        assert_eq!(
            serde_json::to_value(&human).unwrap(),
            serde_json::json!({"class": "human"})
        );
        let keep = ArrivalClass::Human {
            disposition: HumanDisposition::SendKeepStop,
        };
        assert_eq!(
            serde_json::to_value(&keep).unwrap(),
            serde_json::json!({"class": "human", "disposition": "send_keep_stop"})
        );
        for class in [
            human,
            keep,
            ArrivalClass::ItemFinished {
                item: ItemSource {
                    request: "background:run-1".into(),
                    kind: ItemKind::Task,
                    label: "lint".into(),
                    policy: FinishPolicy::Hold,
                },
            },
            ArrivalClass::TimerExpired { stop: 3 },
            ArrivalClass::ScheduledResume { item: None },
            ArrivalClass::Continuation,
            ArrivalClass::SystemNotice,
        ] {
            let text = serde_json::to_string(&class).unwrap();
            assert_eq!(serde_json::from_str::<ArrivalClass>(&text).unwrap(), class);
        }
        assert!(serde_json::from_str::<ArrivalClass>(r#"{"class":"human","extra":1}"#).is_err());
    }

    #[test]
    fn only_messages_and_answers_carry_human_authority() {
        let item = ItemSource {
            request: "question:1".into(),
            kind: ItemKind::Question,
            label: "density".into(),
            policy: FinishPolicy::Wake,
        };
        assert!(
            ArrivalClass::Human {
                disposition: HumanDisposition::Queue
            }
            .is_human()
        );
        assert!(ArrivalClass::Answer { item: item.clone() }.is_human());
        assert!(!ArrivalClass::ItemFinished { item }.is_human());
        assert!(!ArrivalClass::SystemNotice.is_human());
        assert!(!ArrivalClass::ScheduledResume { item: None }.is_human());
    }
}
