//! Session-work points inside the turn loops: the safe-point workflow
//! restore, the turn's beginning and end, the tool gate under a stop
//! declaration, and the natural end of a turn with its corrections. Both
//! turn loops call these same functions.
use super::Agent;
use crate::instruction::notification::Notification;
use crate::message::{ContentBlock, Role, ToolCall};
use crate::session_work::stop::{self, Correction, EndDecision, Gate, NaturalEnd};
use anyhow::{Context, Result};
use jcode_session_types::ContextDeliveryChannel;
use jcode_session_work_types::{FailureClass, TurnOutcome};

/// Where a turn is, for classifying a failure that ends it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TurnPhase {
    /// Before a provider request is sent: preflight, Startup Context, tool
    /// set and prompt preparation.
    #[default]
    Preparing,
    /// A provider request is open.
    Requesting,
    /// After the response: tools and persistence.
    Tools,
}

/// What the turn loop does with one tool call.
pub(super) enum ToolGate {
    Run,
    /// Run, and append the warning to its result.
    RunWarned(String),
    /// Do not run; the refusal is its result. `end_turn` ends the turn after
    /// this result.
    Refuse {
        text: String,
        end_turn: bool,
    },
}

/// What the turn loop does at a natural end.
pub(super) enum TurnEnd {
    /// The turn is over.
    End,
    /// A correction was appended; send another request.
    Continue,
}

impl Agent {
    fn working_dir_path(&self) -> Option<&std::path::Path> {
        self.session
            .working_dir
            .as_deref()
            .map(std::path::Path::new)
    }

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
                .render(self.working_dir_path())?;
            self.add_context_delivery(ContextDeliveryChannel::SessionWork, &notice);
            self.session.save()?;
        }
        Ok(())
    }

    /// Open or continue the session's turn. A turn interrupted by a reload
    /// or crash is continued with its counters.
    pub(super) fn begin_session_work_turn(&mut self) -> Result<()> {
        self.turn_phase = TurnPhase::Preparing;
        if self.session.session_work.is_none() {
            return Ok(());
        }
        crate::session_work::SessionWorkStore::new()
            .begin_turn(&self.session.id, chrono::Utc::now())
            .context("Session work is unavailable for this session")?;
        Ok(())
    }

    /// Record how a turn ended when it did not reach a natural end: a
    /// cancellation, a reload or a failure. Natural ends record their own
    /// outcome. Recording never replaces the turn's own error.
    pub(super) fn finish_session_work_turn(&mut self, error: Option<&anyhow::Error>) {
        if self.session.session_work.is_none() {
            return;
        }
        let store = crate::session_work::SessionWorkStore::new();
        let open = match store.open_turn(&self.session.id) {
            Ok(open) => open,
            Err(store_error) => {
                crate::logging::warn(&format!(
                    "Session work: turn outcome for {} not recorded: {store_error}",
                    self.session.id
                ));
                return;
            }
        };
        if open.is_none() {
            return;
        }
        let outcome = if let Some(error) = error {
            TurnOutcome::Failed {
                class: self.failure_class(error),
            }
        } else if self.is_graceful_shutdown() {
            match self.graceful_shutdown.stop_cause() {
                Some(jcode_tool_types::StopCause::ReloadQuiescence) => {
                    TurnOutcome::InterruptedByReload
                }
                _ => TurnOutcome::InterruptedByUser,
            }
        } else {
            crate::logging::warn(&format!(
                "Session work: turn of {} ended without an outcome",
                self.session.id
            ));
            return;
        };
        self.record_session_work_outcome(&outcome, &uuid::Uuid::new_v4().to_string());
    }

    fn failure_class(&self, error: &anyhow::Error) -> FailureClass {
        if error
            .chain()
            .any(|cause| cause.is::<crate::session::StartupContextDispatchError>())
        {
            return FailureClass::Preflight;
        }
        match self.turn_phase {
            TurnPhase::Preparing => FailureClass::Preflight,
            TurnPhase::Requesting => FailureClass::Provider,
            TurnPhase::Tools => FailureClass::Other,
        }
    }

    fn record_session_work_outcome(&self, outcome: &TurnOutcome, request: &str) {
        let store = crate::session_work::SessionWorkStore::new();
        if let Err(error) = stop::record_outcome(
            &store,
            &self.session.id,
            request,
            outcome,
            chrono::Utc::now(),
        ) {
            crate::logging::warn(&format!(
                "Session work: turn outcome for {} not recorded: {error}",
                self.session.id
            ));
            return;
        }
        if let TurnOutcome::StoppedWithoutDeclaration { cause } = outcome {
            let counter = match cause {
                jcode_session_work_types::ExhaustedCounter::Missing => "missing declarations",
                jcode_session_work_types::ExhaustedCounter::Invalid => "invalid declarations",
                jcode_session_work_types::ExhaustedCounter::Refused => "refused tool calls",
            };
            crate::bus::Bus::global().publish(crate::bus::BusEvent::UiActivity(
                crate::bus::UiActivity::background(
                    Some(self.session.id.clone()),
                    format!(
                        "The turn ended without a valid stop declaration: the limit for {counter} was reached."
                    ),
                    Some("Stopped without a declaration".to_string()),
                ),
            ));
        }
    }

    /// Decide one tool call under the session's stop declaration.
    pub(super) fn session_work_gate(&self, call: &ToolCall) -> Result<ToolGate> {
        if self.session.session_work.is_none() {
            return Ok(ToolGate::Run);
        }
        let store = crate::session_work::SessionWorkStore::new();
        let limits = crate::config::config().session_work.corrections;
        let gate = stop::gate(&store, &self.session.id, &call.name, &call.id, &limits)
            .context("Session work is unavailable for this session")?;
        let working_dir = self.working_dir_path();
        Ok(match gate {
            Gate::Run => ToolGate::Run,
            Gate::RunWarned => {
                ToolGate::RunWarned(Notification::SessionWorkGateWarned.render(working_dir)?)
            }
            Gate::Refuse {
                wait,
                batch,
                end_turn,
            } => ToolGate::Refuse {
                text: if batch {
                    Notification::SessionWorkGateRefusedBatch
                } else if wait {
                    Notification::SessionWorkGateRefusedWait
                } else {
                    Notification::SessionWorkGateRefused
                }
                .render(working_dir)?,
                end_turn,
            },
        })
    }

    /// End a turn whose refused-call counter ran out. The declaration stays
    /// and becomes effective if it still holds.
    pub(super) async fn end_session_work_turn_after_refusals(&mut self) -> Result<()> {
        let store = crate::session_work::SessionWorkStore::new();
        let work = crate::tool::declare_stop::background_work(&self.session.id).await;
        let now = chrono::Utc::now();
        let items = stop::observe_items(&store, &self.session.id, &work, now)?;
        let outcome = stop::end_after_refusals(&store, &self.session.id, &items, now)?;
        self.record_session_work_outcome(&outcome, &uuid::Uuid::new_v4().to_string());
        Ok(())
    }

    /// The natural end of a turn: no tool calls and nothing left to inject.
    /// Settle a declaration, allow implicit completion, or append a
    /// correction and continue while its counter lasts. Refusals, truncated
    /// and empty output are recorded but never corrected.
    pub(super) async fn session_work_natural_end(
        &mut self,
        stop_reason: Option<&str>,
        final_text: &str,
        final_message_id: Option<&str>,
    ) -> Result<TurnEnd> {
        if self.session.session_work.is_none() {
            return Ok(TurnEnd::End);
        }
        let nonvoluntary = if Self::is_guardrail_stop_reason(stop_reason) {
            Some(TurnOutcome::ProviderRefusal)
        } else if stop_reason.is_some_and(Self::should_continue_after_stop_reason)
            || Self::is_stranded_tool_use_stop(stop_reason)
        {
            Some(TurnOutcome::Truncated)
        } else {
            None
        };
        let occurrence = final_message_id
            .map(str::to_string)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let store = crate::session_work::SessionWorkStore::new();
        let work = crate::tool::declare_stop::background_work(&self.session.id).await;
        let now = chrono::Utc::now();
        let items = stop::observe_items(&store, &self.session.id, &work, now)
            .context("Session work is unavailable for this session")?;
        let limits = crate::config::config().session_work.corrections;
        let decision = stop::natural_end(
            &store,
            &self.session.id,
            &NaturalEnd {
                final_text,
                occurrence: &occurrence,
                nonvoluntary,
                empty: final_text.trim().is_empty(),
            },
            &items,
            &limits,
            now,
        )
        .context("Session work is unavailable for this session")?;
        match decision {
            EndDecision::End(outcome) => {
                self.record_session_work_outcome(&outcome, &format!("end:{occurrence}"));
                Ok(TurnEnd::End)
            }
            EndDecision::Correct(correction) => {
                let working_dir = self.working_dir_path();
                let text = match &correction {
                    Correction::Missing { unfinished } => {
                        Notification::SessionWorkCorrectionMissing {
                            modules: &stop::module_list(unfinished),
                        }
                        .render(working_dir)?
                    }
                    Correction::Invalid(refusal) => {
                        crate::tool::declare_stop::refusal_text(refusal, working_dir)?
                    }
                };
                self.add_context_delivery(ContextDeliveryChannel::StopCorrection, &text);
                self.session.save()?;
                Ok(TurnEnd::Continue)
            }
        }
    }

    /// The result of a refused tool call, appended in place of running it.
    pub(super) fn append_refused_tool_result(&mut self, call: &ToolCall, text: &str) {
        self.add_message(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: call.id.clone(),
                content: text.to_string(),
                is_error: Some(true),
            }],
        );
    }
}
