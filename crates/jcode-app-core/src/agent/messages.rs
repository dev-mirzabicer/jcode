use super::*;

impl Agent {
    pub(crate) fn add_user_message_with_origin(
        &mut self,
        content: Vec<ContentBlock>,
        display_role: Option<StoredDisplayRole>,
        origin: Option<jcode_session_types::StoredMessageOrigin>,
    ) -> Result<String> {
        let id = self
            .session
            .add_user_message_with_origin(content, display_role, origin)?;
        self.record_context_runtime_message_added();
        Ok(id)
    }
    /// Append model-visible harness context as persisted transcript content
    /// (INT-01, D2). The caller persists it with the operation it belongs to.
    pub(crate) fn add_context_delivery(
        &mut self,
        channel: jcode_session_types::ContextDeliveryChannel,
        body: &str,
    ) -> Option<String> {
        let id = self.session.append_context_delivery(channel, body)?;
        self.record_context_runtime_message_added();
        Some(id)
    }

    pub(crate) fn add_tool_set_delivery(
        &mut self,
        body: &str,
        changes: Vec<jcode_message_types::ToolSetChange>,
    ) -> Option<String> {
        let id = self.session.append_tool_set_delivery(body, changes)?;
        self.record_context_runtime_message_added();
        Some(id)
    }

    pub(crate) fn add_message(&mut self, role: Role, content: Vec<ContentBlock>) -> String {
        let id = self.session.add_message(role, content);
        self.record_context_runtime_message_added();
        id
    }

    pub(crate) fn add_message_with_display_role(
        &mut self,
        role: Role,
        content: Vec<ContentBlock>,
        display_role: Option<StoredDisplayRole>,
    ) -> String {
        let id = self
            .session
            .add_message_with_display_role(role, content, display_role);
        self.record_context_runtime_message_added();
        id
    }

    pub(crate) fn add_message_with_duration(
        &mut self,
        role: Role,
        content: Vec<ContentBlock>,
        duration_ms: Option<u64>,
    ) -> String {
        let id = self
            .session
            .add_message_with_duration(role, content, duration_ms);
        self.record_context_runtime_message_added();
        id
    }

    pub(crate) fn add_message_ext(
        &mut self,
        role: Role,
        content: Vec<ContentBlock>,
        duration_ms: Option<u64>,
        token_usage: Option<crate::session::StoredTokenUsage>,
    ) -> String {
        let id = self
            .session
            .add_message_ext(role, content, duration_ms, token_usage);
        self.record_context_runtime_message_added();
        id
    }
}
