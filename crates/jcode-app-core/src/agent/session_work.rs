//! Session-work safe points inside the turn loops.
use super::Agent;
use anyhow::{Context, Result};

impl Agent {
    /// Before each provider request, bring an activated session's workflow
    /// file back to its store. A file changed outside the native file tools is
    /// restored and the session is told once, through an appended notice. An
    /// unreadable store stops the turn with a visible error instead of letting
    /// the session continue as if it had no workflow.
    pub(crate) fn reconcile_session_work(&mut self) -> Result<()> {
        if self.session.session_work.is_none() {
            return Ok(());
        }
        let restored = crate::session_work::reconcile_session(&self.session)
            .context("Session work is unavailable for this session")?;
        if let Some(revision) = restored {
            let notice =
                crate::instruction::notification::Notification::SessionWorkWorkflowRestored {
                    revision,
                }
                .render(
                    self.session
                        .working_dir
                        .as_deref()
                        .map(std::path::Path::new),
                )?;
            self.add_context_delivery(
                jcode_session_types::ContextDeliveryChannel::SessionWork,
                &notice,
            );
            self.session.save()?;
        }
        Ok(())
    }
}
