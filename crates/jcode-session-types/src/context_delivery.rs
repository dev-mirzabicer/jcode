//! Model-visible harness context delivered as persisted transcript content.
//!
//! Dynamic context (per-turn system reminders, the batch nudge) reaches the
//! model only as an appended user-role message carrying this origin. Once
//! delivered it is ordinary authoritative history: never rewritten, moved or
//! removed, so every provider request stays an append of the previous one
//! (INT-01, INV-1). The text form is the same for every provider.

use jcode_message_types::{ContentBlock, Role};
use serde::{Deserialize, Serialize};

use crate::{StoredMessage, StoredMessageOrigin};

/// Which harness channel produced a delivery.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextDeliveryChannel {
    /// The system reminder that accompanies one turn or one injected input.
    /// A reminder-only turn, such as a reload resume, has it as its content.
    TurnReminder,
    /// The batch-tool nudge after repeated single-tool rounds.
    BatchNudge,
}

/// Persisted identity of one delivery: its channel and the fingerprint of the
/// exact delivered text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredContextDelivery {
    pub channel: ContextDeliveryChannel,
    pub fingerprint: String,
}

const OPEN: &str = "<system-reminder>\n";
const CLOSE: &str = "\n</system-reminder>";
const REMINDER_HEADING: &str = "# System Reminder\n\n";

/// The provider-visible text of a delivery, or `None` when there is nothing
/// to deliver. Turn reminders keep the heading they had inside the former
/// per-request system context, so GPT receives the same bytes as before.
pub fn context_delivery_text(channel: ContextDeliveryChannel, body: &str) -> Option<String> {
    let body = body.trim();
    if body.is_empty() {
        return None;
    }
    let heading = match channel {
        ContextDeliveryChannel::TurnReminder => REMINDER_HEADING,
        ContextDeliveryChannel::BatchNudge => "",
    };
    Some(format!("{OPEN}{heading}{body}{CLOSE}"))
}

/// Build the stored message for one delivery. The caller appends and persists
/// it together with the operation it belongs to.
pub fn context_delivery_message(
    id: String,
    channel: ContextDeliveryChannel,
    body: &str,
    timestamp: chrono::DateTime<chrono::Utc>,
) -> Option<StoredMessage> {
    let text = context_delivery_text(channel, body)?;
    let fingerprint = jcode_message_types::stable_text_fingerprint(&text);
    Some(StoredMessage {
        id,
        role: Role::User,
        content: vec![ContentBlock::Text {
            text,
            cache_control: None,
        }],
        display_role: Some(crate::StoredDisplayRole::System),
        timestamp: Some(timestamp),
        tool_duration_ms: None,
        token_usage: None,
        origin: Some(StoredMessageOrigin::ContextDelivery(
            StoredContextDelivery {
                channel,
                fingerprint,
            },
        )),
    })
}

impl StoredMessage {
    /// The channel and human-readable body of a delivery. Like composed
    /// origins, metadata that no longer matches the stored text (for example
    /// after export redaction) is not trusted, and the message is then plain
    /// source.
    pub fn context_delivery(&self) -> Option<(ContextDeliveryChannel, &str)> {
        let StoredMessageOrigin::ContextDelivery(delivery) = self.origin.as_ref()? else {
            return None;
        };
        let [ContentBlock::Text { text, .. }] = self.content.as_slice() else {
            return None;
        };
        if self.role != Role::User
            || jcode_message_types::stable_text_fingerprint(text) != delivery.fingerprint
        {
            return None;
        }
        let body = text.strip_prefix(OPEN)?.strip_suffix(CLOSE)?;
        Some((delivery.channel, body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delivered(channel: ContextDeliveryChannel, body: &str) -> StoredMessage {
        context_delivery_message("message_1".into(), channel, body, chrono::Utc::now()).unwrap()
    }

    #[test]
    fn delivery_text_is_one_form_for_every_provider() {
        assert_eq!(
            context_delivery_text(ContextDeliveryChannel::TurnReminder, "  keep going \n"),
            Some("<system-reminder>\n# System Reminder\n\nkeep going\n</system-reminder>".into())
        );
        assert_eq!(
            context_delivery_text(ContextDeliveryChannel::BatchNudge, "use batch"),
            Some("<system-reminder>\nuse batch\n</system-reminder>".into())
        );
        assert_eq!(
            context_delivery_text(ContextDeliveryChannel::TurnReminder, " \n "),
            None
        );
    }

    #[test]
    fn delivery_round_trips_through_serialization_and_validates_its_text() {
        let message = delivered(ContextDeliveryChannel::TurnReminder, "synthetic reminder");
        assert_eq!(message.display_role, Some(crate::StoredDisplayRole::System));
        let decoded: StoredMessage =
            serde_json::from_str(&serde_json::to_string(&message).unwrap()).unwrap();
        assert_eq!(
            decoded.context_delivery(),
            Some((
                ContextDeliveryChannel::TurnReminder,
                "# System Reminder\n\nsynthetic reminder"
            ))
        );
        // The provider never sees origin metadata.
        assert_eq!(
            serde_json::to_value(decoded.to_message().content).unwrap(),
            serde_json::to_value(&message.content).unwrap()
        );
    }

    #[test]
    fn changed_text_is_no_longer_trusted_as_a_delivery() {
        let mut message = delivered(ContextDeliveryChannel::BatchNudge, "synthetic nudge");
        message.content = vec![ContentBlock::Text {
            text: "<system-reminder>\n[REDACTED]\n</system-reminder>".into(),
            cache_control: None,
        }];
        assert_eq!(message.context_delivery(), None);
    }
}
