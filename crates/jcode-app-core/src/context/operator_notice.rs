//! Operator authority for delivered harness context on the request path
//! (INT-01/WP-06, D17).
//!
//! A stored delivery that asks for operator authority keeps its stored text.
//! When the request goes to a runtime that renders operator messages, that
//! delivery's projected message is sent as `ContentBlock::OperatorNotice`
//! instead, and the runtime decides the native form from its model and the
//! message's position. Every other runtime receives the stored text, so its
//! requests are byte-identical to the ones before WP-06.

use crate::message::{ContentBlock, Message, Role};
use crate::provider::Provider;
use jcode_session_types::StoredMessage;
use std::collections::HashMap;

/// `projected` with each operator delivery of `stored` as an operator notice,
/// when `provider` renders them. A projected message is a delivery when it is
/// a user message whose only block is the delivery's exact stored text; the
/// text is identified by the stored origin's fingerprint, never by its prose.
pub fn with_operator_notices(
    provider: &dyn Provider,
    stored: &[StoredMessage],
    mut projected: Vec<Message>,
) -> Vec<Message> {
    if !provider.renders_operator_notices() {
        return projected;
    }
    let notices: HashMap<&str, &str> = stored
        .iter()
        .filter_map(StoredMessage::operator_delivery)
        .map(|delivery| (delivery.text, delivery.body))
        .collect();
    if notices.is_empty() {
        return projected;
    }
    for message in &mut projected {
        if message.role != Role::User {
            continue;
        }
        let [ContentBlock::Text { text, .. }] = message.content.as_slice() else {
            continue;
        };
        if let Some(body) = notices.get(text.as_str()) {
            message.content = vec![ContentBlock::OperatorNotice {
                text: text.clone(),
                body: (*body).to_string(),
                tool_changes: Vec::new(),
            }];
        }
    }
    projected
}
