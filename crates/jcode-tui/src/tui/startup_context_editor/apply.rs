use super::*;
use crate::protocol::{
    StartupContextApplyPhase, StartupContextApplyTargetState, StartupContextFailureKind,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ApplyIntent {
    SessionOnly,
    SessionAndProjectDefault,
}

impl ApplyIntent {
    pub(super) fn save_project_default(self) -> bool {
        matches!(self, Self::SessionAndProjectDefault)
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::SessionOnly => "Use in this session",
            Self::SessionAndProjectDefault => "Use in this session + save as project default",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ApplySelectionPurpose {
    DraftValidation,
    Apply(ApplyIntent),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ExternalTargetConfirmation {
    pub(super) local_id: u64,
    pub(super) logical_path: String,
    pub(super) approved_target: Option<String>,
    pub(super) resolved_target: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ApplyReview {
    pub(super) intent: ApplyIntent,
    pub(super) draft_generation: u64,
    pub(super) plan_revision: u64,
    pub(super) project_key_digest: String,
    pub(super) selection: Vec<StartupContextSelectionInput>,
    pub(super) selected_count: usize,
    pub(super) aggregate_bytes: u64,
    pub(super) aggregate_estimated_tokens: u64,
    pub(super) external_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ApplyOverlay {
    Previewing {
        intent: ApplyIntent,
    },
    ConfirmExternal {
        intent: ApplyIntent,
        draft_generation: u64,
        targets: Vec<ExternalTargetConfirmation>,
    },
    Review(ApplyReview),
    PreviewFailed {
        intent: ApplyIntent,
        failure: StartupContextFailure,
    },
    ValidationIssues {
        intent: ApplyIntent,
    },
    Status,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ApplyTracking {
    pub(super) operation_id: String,
    pub(super) intent: ApplyIntent,
    pub(super) draft_generation: u64,
    pub(super) expected_plan_revision: u64,
    pub(super) selection: Vec<StartupContextSelectionInput>,
    pub(super) status: Option<StartupContextApplyStatus>,
    pub(super) failure: Option<StartupContextFailure>,
}

impl ApplyTracking {
    pub(super) fn is_non_terminal(&self) -> bool {
        self.status.as_ref().is_none_or(|status| {
            !matches!(
                status.phase,
                StartupContextApplyPhase::Succeeded
                    | StartupContextApplyPhase::Failed
                    | StartupContextApplyPhase::Canceled
            )
        }) && self.failure.is_none()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum EditorAuthorityRefresh {
    #[default]
    None,
    CloseQueued,
    ReopenQueued,
}

impl StartupContextEditor {
    pub(super) fn install_debug_apply_fixture(&mut self, name: &str) -> bool {
        let intent = ApplyIntent::SessionAndProjectDefault;
        let review = || ApplyReview {
            intent,
            draft_generation: self.draft_generation,
            plan_revision: self.lease().map_or(7, |lease| lease.plan_revision),
            project_key_digest: self.lease().map_or_else(
                || "fixture-project".to_string(),
                |lease| lease.project_key_digest.clone(),
            ),
            selection: self.draft.iter().map(|entry| entry.input.clone()).collect(),
            selected_count: self.draft.len(),
            aggregate_bytes: self.draft.iter().filter_map(|entry| entry.bytes).sum(),
            aggregate_estimated_tokens: self
                .draft
                .iter()
                .filter_map(|entry| entry.estimated_tokens)
                .sum(),
            external_count: self
                .draft
                .iter()
                .filter(|entry| {
                    entry.classification == Some(StartupContextPathClassification::External)
                })
                .count(),
        };
        if matches!(name, "editor-apply-review" | "editor-apply-review-late") {
            self.apply_overlay = Some(ApplyOverlay::Review(review()));
            return true;
        }
        if name == "editor-apply-external" {
            let targets = self.external_targets_requiring_confirmation();
            self.apply_overlay = Some(ApplyOverlay::ConfirmExternal {
                intent,
                draft_generation: self.draft_generation,
                targets,
            });
            return true;
        }
        let phase = match name {
            "editor-apply-queued" => StartupContextApplyPhase::Queued,
            "editor-apply-applying" => StartupContextApplyPhase::Applying,
            "editor-apply-recovery" => StartupContextApplyPhase::RecoveryRequired,
            "editor-apply-success" => StartupContextApplyPhase::Succeeded,
            "editor-apply-partial" => StartupContextApplyPhase::RecoveryRequired,
            "editor-apply-failed" => StartupContextApplyPhase::Failed,
            "editor-apply-canceled" => StartupContextApplyPhase::Canceled,
            _ => return false,
        };
        let now = chrono::Utc::now();
        let session_target = match name {
            "editor-apply-success" => StartupContextApplyTargetState::Applied { revision: None },
            "editor-apply-partial" => StartupContextApplyTargetState::Failed {
                message: "session persistence is retrying from durable recovery".to_string(),
                retryable: true,
            },
            "editor-apply-failed" => StartupContextApplyTargetState::Failed {
                message: "selected file disappeared before late capture".to_string(),
                retryable: true,
            },
            "editor-apply-canceled" => StartupContextApplyTargetState::Canceled,
            _ => StartupContextApplyTargetState::Pending,
        };
        let project_default_target = match name {
            "editor-apply-success" | "editor-apply-partial" => {
                StartupContextApplyTargetState::Applied { revision: Some(8) }
            }
            "editor-apply-failed" => StartupContextApplyTargetState::Failed {
                message: "project default was not changed".to_string(),
                retryable: true,
            },
            "editor-apply-canceled" => StartupContextApplyTargetState::Canceled,
            _ => StartupContextApplyTargetState::Pending,
        };
        let failure = match name {
            "editor-apply-partial" => Some(StartupContextFailure {
                operation: crate::protocol::StartupContextOperation::ApplySelection,
                kind: StartupContextFailureKind::Recovery,
                message: "Project default committed; session target requires recovery".to_string(),
                retryable: true,
                issues: Vec::new(),
            }),
            "editor-apply-failed" => Some(StartupContextFailure {
                operation: crate::protocol::StartupContextOperation::ApplySelection,
                kind: StartupContextFailureKind::Io,
                message: "Late addition failed atomically; the established session is unchanged"
                    .to_string(),
                retryable: true,
                issues: Vec::new(),
            }),
            _ => None,
        };
        let status = StartupContextApplyStatus {
            operation_id: "fixture-startup-apply".to_string(),
            session_id: self.session_id.clone(),
            phase,
            session_target,
            project_default_target,
            batch_id: (phase == StartupContextApplyPhase::Succeeded)
                .then(|| "fixture-late-batch".to_string()),
            file_count: self.draft.len(),
            created_at: now,
            updated_at: now,
            failure,
        };
        self.apply_tracking = Some(ApplyTracking {
            operation_id: status.operation_id.clone(),
            intent,
            draft_generation: self.draft_generation,
            expected_plan_revision: 7,
            selection: self.draft.iter().map(|entry| entry.input.clone()).collect(),
            status: Some(status),
            failure: None,
        });
        self.apply_overlay = Some(ApplyOverlay::Status);
        true
    }

    pub(super) fn begin_apply(&mut self, intent: ApplyIntent) {
        if !matches!(self.phase, EditorPhase::Ready) {
            self.notice = Some("Startup Context editor is not ready to apply changes".to_string());
            return;
        }
        if self
            .apply_tracking
            .as_ref()
            .is_some_and(ApplyTracking::is_non_terminal)
        {
            self.apply_overlay = Some(ApplyOverlay::Status);
            self.notice = Some("A Startup Context apply is already in progress".to_string());
            return;
        }
        self.apply_tracking = None;
        self.apply_overlay = Some(ApplyOverlay::Previewing { intent });
        self.notice = None;
        self.queue_selection_preview_for(ApplySelectionPurpose::Apply(intent));
    }

    pub(super) fn finish_apply_preview(
        &mut self,
        intent: ApplyIntent,
        selected_count: usize,
        issue_count: usize,
        aggregate_bytes: u64,
        aggregate_estimated_tokens: u64,
        ignored_duplicates: usize,
    ) {
        if ignored_duplicates > 0 {
            self.apply_overlay = Some(ApplyOverlay::ValidationIssues { intent });
            return;
        }
        let external_targets = self.external_targets_requiring_confirmation();
        let external_issue_count = external_targets.len();
        if issue_count > external_issue_count || !self.batch_issues.is_empty() {
            self.apply_overlay = Some(ApplyOverlay::ValidationIssues { intent });
            return;
        }
        if !external_targets.is_empty() {
            self.apply_overlay = Some(ApplyOverlay::ConfirmExternal {
                intent,
                draft_generation: self.draft_generation,
                targets: external_targets,
            });
            return;
        }
        let Some(lease) = self.lease().cloned() else {
            self.apply_overlay = Some(ApplyOverlay::PreviewFailed {
                intent,
                failure: StartupContextFailure {
                    operation: crate::protocol::StartupContextOperation::PreviewSelection,
                    kind: StartupContextFailureKind::LeaseNotFound,
                    message: "Startup Context editor lease is unavailable".to_string(),
                    retryable: true,
                    issues: Vec::new(),
                },
            });
            return;
        };
        if selected_count != self.draft.len() {
            self.apply_overlay = Some(ApplyOverlay::ValidationIssues { intent });
            return;
        }
        let external_count = self
            .draft
            .iter()
            .filter(|entry| {
                entry.classification == Some(StartupContextPathClassification::External)
            })
            .count();
        self.apply_overlay = Some(ApplyOverlay::Review(ApplyReview {
            intent,
            draft_generation: self.draft_generation,
            plan_revision: lease.plan_revision,
            project_key_digest: lease.project_key_digest,
            selection: self.draft.iter().map(|entry| entry.input.clone()).collect(),
            selected_count,
            aggregate_bytes,
            aggregate_estimated_tokens,
            external_count,
        }));
    }

    fn external_targets_requiring_confirmation(&self) -> Vec<ExternalTargetConfirmation> {
        self.draft
            .iter()
            .filter_map(|entry| {
                let issue = entry.issue.as_ref()?;
                match &issue.kind {
                    StartupContextFileIssueKind::ExternalApprovalRequired { resolved_target } => {
                        Some(ExternalTargetConfirmation {
                            local_id: entry.local_id,
                            logical_path: entry.logical_path.clone(),
                            approved_target: None,
                            resolved_target: resolved_target.clone(),
                        })
                    }
                    StartupContextFileIssueKind::ExternalTargetChanged {
                        approved_target,
                        resolved_target,
                    } => Some(ExternalTargetConfirmation {
                        local_id: entry.local_id,
                        logical_path: entry.logical_path.clone(),
                        approved_target: Some(approved_target.clone()),
                        resolved_target: resolved_target.clone(),
                    }),
                    _ => None,
                }
            })
            .collect()
    }

    pub(super) fn confirm_apply_overlay(&mut self) {
        let Some(overlay) = self.apply_overlay.clone() else {
            return;
        };
        match overlay {
            ApplyOverlay::ConfirmExternal {
                intent,
                draft_generation,
                targets,
            } => {
                if draft_generation != self.draft_generation {
                    self.apply_overlay = None;
                    self.notice = Some(
                        "Draft changed while external confirmation was open; review it again"
                            .to_string(),
                    );
                    return;
                }
                for target in targets {
                    let Some(entry) = self
                        .draft
                        .iter_mut()
                        .find(|entry| entry.local_id == target.local_id)
                    else {
                        self.apply_overlay = None;
                        self.notice = Some(
                            "External confirmation no longer matches the current draft".to_string(),
                        );
                        return;
                    };
                    entry.input.approved_external_target = Some(target.resolved_target);
                    entry.issue = None;
                }
                self.draft_generation = self.draft_generation.saturating_add(1);
                self.apply_overlay = Some(ApplyOverlay::Previewing { intent });
                self.queue_selection_preview_for(ApplySelectionPurpose::Apply(intent));
            }
            ApplyOverlay::Review(review) => self.submit_apply(review),
            ApplyOverlay::PreviewFailed { intent, failure } if failure.retryable => {
                self.begin_apply(intent)
            }
            ApplyOverlay::ValidationIssues { intent } => self.begin_apply(intent),
            ApplyOverlay::PreviewFailed { .. } => {}
            ApplyOverlay::Previewing { .. } | ApplyOverlay::Status => {}
        }
    }

    fn submit_apply(&mut self, review: ApplyReview) {
        let Some(lease) = self.lease().cloned() else {
            self.notice = Some("Startup Context editor lease is unavailable".to_string());
            return;
        };
        if review.draft_generation != self.draft_generation
            || review.plan_revision != lease.plan_revision
            || review.project_key_digest != lease.project_key_digest
            || review.selection
                != self
                    .draft
                    .iter()
                    .map(|entry| entry.input.clone())
                    .collect::<Vec<_>>()
        {
            self.apply_overlay = None;
            self.notice = Some(
                "Draft or editor revision changed; request a new authoritative apply preview"
                    .to_string(),
            );
            return;
        }
        let operation_id = crate::id::new_id("startup_apply");
        self.apply_tracking = Some(ApplyTracking {
            operation_id: operation_id.clone(),
            intent: review.intent,
            draft_generation: review.draft_generation,
            expected_plan_revision: review.plan_revision,
            selection: review.selection.clone(),
            status: None,
            failure: None,
        });
        self.apply_overlay = Some(ApplyOverlay::Status);
        self.queued
            .push_back(StartupContextEditorAction::ApplySelection {
                lease,
                operation_id,
                selection: review.selection,
                save_project_default: review.intent.save_project_default(),
                draft_generation: review.draft_generation,
            });
    }

    pub(super) fn dismiss_apply_overlay(&mut self) {
        self.apply_overlay = None;
    }

    pub(super) fn show_apply_status(&mut self) {
        if self.apply_tracking.is_some() {
            self.apply_overlay = Some(ApplyOverlay::Status);
        }
    }

    pub(super) fn cancel_queued_apply(&mut self) {
        let Some(tracking) = self.apply_tracking.as_ref() else {
            return;
        };
        let Some(status) = tracking.status.as_ref() else {
            return;
        };
        if status.phase != StartupContextApplyPhase::Queued {
            self.notice = Some("Only a queued Startup Context apply can be canceled".to_string());
            return;
        }
        let Some(lease) = self.lease().cloned() else {
            self.notice = Some("Reopen the editor lease before canceling this queue".to_string());
            return;
        };
        self.queued
            .push_back(StartupContextEditorAction::CancelApply {
                lease,
                operation_id: tracking.operation_id.clone(),
            });
    }

    pub(super) fn refresh_apply_status(&mut self) {
        let Some(tracking) = self.apply_tracking.as_ref() else {
            return;
        };
        self.queued
            .push_back(StartupContextEditorAction::GetApplyStatus {
                operation_id: tracking.operation_id.clone(),
            });
    }

    pub(super) fn retry_apply(&mut self) {
        let Some(tracking) = self.apply_tracking.clone() else {
            return;
        };
        if tracking
            .status
            .as_ref()
            .is_some_and(|status| status.phase == StartupContextApplyPhase::Failed)
        {
            self.apply_tracking = None;
            self.apply_overlay = None;
            self.begin_apply(tracking.intent);
            return;
        }
        let Some(lease) = self.lease().cloned() else {
            self.notice = Some("Reopen the editor lease before retrying apply".to_string());
            return;
        };
        if lease.plan_revision != tracking.expected_plan_revision {
            self.apply_tracking = None;
            self.apply_overlay = None;
            self.notice = Some(
                "Project default changed; review the preserved draft against the new revision"
                    .to_string(),
            );
            self.request_authority_refresh();
            return;
        }
        if tracking.failure.as_ref().is_some_and(|failure| {
            !failure.retryable && failure.kind != StartupContextFailureKind::ApplyNotFound
        }) {
            self.notice = Some("This apply failure is not retryable".to_string());
            return;
        }
        if tracking.status.as_ref().is_some_and(|status| {
            matches!(
                status.phase,
                StartupContextApplyPhase::Succeeded | StartupContextApplyPhase::Canceled
            )
        }) {
            return;
        }
        if let Some(current) = self.apply_tracking.as_mut() {
            current.failure = None;
        }
        self.queued
            .push_back(StartupContextEditorAction::ApplySelection {
                lease,
                operation_id: tracking.operation_id,
                selection: tracking.selection,
                save_project_default: tracking.intent.save_project_default(),
                draft_generation: tracking.draft_generation,
            });
    }

    pub(crate) fn accept_apply_status(
        &mut self,
        id: u64,
        status: StartupContextApplyStatus,
    ) -> bool {
        if status.session_id != self.session_id {
            return false;
        }
        let Some(tracking) = self.apply_tracking.as_ref() else {
            return false;
        };
        if tracking.operation_id != status.operation_id {
            return false;
        }
        if id != 0 {
            let Some(pending) = self.pending.remove(&id) else {
                return false;
            };
            match pending {
                StartupContextPendingRequest::Apply {
                    operation_id,
                    lease_id,
                    project_key_digest,
                    expected_plan_revision,
                    draft_generation,
                } => {
                    let Some(lease) = self.lease() else {
                        return false;
                    };
                    if operation_id != status.operation_id
                        || lease_id != lease.lease_id
                        || project_key_digest != lease.project_key_digest
                        || expected_plan_revision != lease.plan_revision
                        || draft_generation != tracking.draft_generation
                    {
                        return false;
                    }
                }
                StartupContextPendingRequest::CancelApply { operation_id }
                | StartupContextPendingRequest::ApplyStatus { operation_id }
                    if operation_id == status.operation_id => {}
                _ => return false,
            }
        }
        let terminal = matches!(
            status.phase,
            StartupContextApplyPhase::Succeeded
                | StartupContextApplyPhase::Failed
                | StartupContextApplyPhase::Canceled
        );
        if let Some(tracking) = self.apply_tracking.as_mut() {
            tracking.failure = None;
            tracking.status = Some(status);
        }
        self.apply_overlay = Some(ApplyOverlay::Status);
        if terminal {
            self.request_authority_refresh();
        }
        true
    }

    pub(super) fn accept_apply_failure(
        &mut self,
        pending: StartupContextPendingRequest,
        failure: StartupContextFailure,
    ) -> bool {
        match pending {
            StartupContextPendingRequest::Selection {
                generation,
                draft_generation,
                purpose: ApplySelectionPurpose::Apply(intent),
                ..
            } if generation == self.selection_generation
                && draft_generation == self.draft_generation =>
            {
                let should_refresh = matches!(
                    failure.kind,
                    StartupContextFailureKind::StalePlanRevision
                        | StartupContextFailureKind::LeaseExpired
                        | StartupContextFailureKind::LeaseNotFound
                );
                self.apply_overlay = Some(ApplyOverlay::PreviewFailed {
                    intent,
                    failure: failure.clone(),
                });
                self.notice = Some(failure.message);
                if should_refresh {
                    self.request_authority_refresh();
                }
                true
            }
            StartupContextPendingRequest::Apply { operation_id, .. }
            | StartupContextPendingRequest::CancelApply { operation_id }
            | StartupContextPendingRequest::ApplyStatus { operation_id } => {
                let Some(tracking) = self.apply_tracking.as_mut() else {
                    return false;
                };
                if tracking.operation_id != operation_id {
                    return false;
                }
                let should_refresh = matches!(
                    failure.kind,
                    StartupContextFailureKind::StalePlanRevision
                        | StartupContextFailureKind::LeaseExpired
                        | StartupContextFailureKind::LeaseNotFound
                );
                tracking.failure = Some(failure.clone());
                self.apply_overlay = Some(ApplyOverlay::Status);
                self.notice = Some(failure.message);
                if should_refresh {
                    self.request_authority_refresh();
                }
                true
            }
            _ => false,
        }
    }

    pub(super) fn request_authority_refresh(&mut self) {
        if !matches!(self.authority_refresh, EditorAuthorityRefresh::None) {
            return;
        }
        let Some(lease) = self.lease().cloned() else {
            return;
        };
        self.authority_refresh = EditorAuthorityRefresh::CloseQueued;
        self.phase = EditorPhase::Closing;
        self.queued.push_back(StartupContextEditorAction::Close {
            lease_id: lease.lease_id,
            project_key_digest: lease.project_key_digest,
        });
    }

    pub(super) fn handle_apply_key(&mut self, code: KeyCode) -> Option<bool> {
        self.apply_overlay.as_ref()?;
        match code {
            KeyCode::Char('q') => {
                self.close();
                Some(true)
            }
            KeyCode::Esc => {
                self.dismiss_apply_overlay();
                Some(false)
            }
            KeyCode::Enter => {
                self.confirm_apply_overlay();
                Some(false)
            }
            KeyCode::Char('c') => {
                self.cancel_queued_apply();
                Some(false)
            }
            KeyCode::Char('r') => {
                if self.apply_tracking.as_ref().is_some_and(|tracking| {
                    tracking
                        .status
                        .as_ref()
                        .is_some_and(|status| status.phase == StartupContextApplyPhase::Failed)
                        || tracking.failure.as_ref().is_some_and(|failure| {
                            failure.retryable
                                || failure.kind == StartupContextFailureKind::ApplyNotFound
                        })
                }) {
                    self.retry_apply();
                } else {
                    self.refresh_apply_status();
                }
                Some(false)
            }
            _ => Some(false),
        }
    }

    pub(super) fn handle_apply_row_action(&mut self, action: RowAction) -> Option<bool> {
        match action {
            RowAction::ApplySession => {
                self.begin_apply(ApplyIntent::SessionOnly);
                Some(false)
            }
            RowAction::ApplyAndSave => {
                self.begin_apply(ApplyIntent::SessionAndProjectDefault);
                Some(false)
            }
            RowAction::ApplyConfirm => {
                self.confirm_apply_overlay();
                Some(false)
            }
            RowAction::ApplyCancelLayer => {
                self.dismiss_apply_overlay();
                Some(false)
            }
            RowAction::ApplyCancelQueued => {
                self.cancel_queued_apply();
                Some(false)
            }
            RowAction::ApplyRetry => {
                self.retry_apply();
                Some(false)
            }
            RowAction::ApplyRefreshStatus => {
                self.refresh_apply_status();
                Some(false)
            }
            RowAction::ApplyShowStatus => {
                self.show_apply_status();
                Some(false)
            }
            _ => None,
        }
    }

    pub(super) fn row_action_allowed_while_layered(&self, action: RowAction) -> bool {
        if self.input_mode.is_some() {
            return false;
        }
        if self.apply_overlay.is_none() {
            return true;
        }
        matches!(
            action,
            RowAction::ApplyConfirm
                | RowAction::ApplyCancelLayer
                | RowAction::ApplyCancelQueued
                | RowAction::ApplyRetry
                | RowAction::ApplyRefreshStatus
                | RowAction::CloseEditor
        )
    }

    pub(super) fn tracked_status(&self) -> Option<&StartupContextApplyStatus> {
        self.apply_tracking
            .as_ref()
            .and_then(|tracking| tracking.status.as_ref())
    }

    pub(super) fn tracked_failure(&self) -> Option<&StartupContextFailure> {
        self.apply_tracking
            .as_ref()
            .and_then(|tracking| tracking.failure.as_ref())
    }

    pub(crate) fn has_tracked_apply(&self) -> bool {
        self.apply_tracking.is_some()
    }

    #[cfg(test)]
    pub(crate) fn apply_preview_saves_default_for_test(&self) -> Option<bool> {
        match self.apply_overlay.as_ref()? {
            ApplyOverlay::Previewing { intent }
            | ApplyOverlay::ConfirmExternal { intent, .. }
            | ApplyOverlay::PreviewFailed { intent, .. }
            | ApplyOverlay::ValidationIssues { intent } => Some(intent.save_project_default()),
            ApplyOverlay::Review(review) => Some(review.intent.save_project_default()),
            ApplyOverlay::Status => self
                .apply_tracking
                .as_ref()
                .map(|tracking| tracking.intent.save_project_default()),
        }
    }

    pub(super) fn reconnect_apply_status(&mut self) {
        if self.apply_tracking.is_some() {
            self.refresh_apply_status();
        }
    }

    pub(super) fn target_state_label(target: &StartupContextApplyTargetState) -> String {
        match target {
            StartupContextApplyTargetState::NotRequested => "not requested".to_string(),
            StartupContextApplyTargetState::Pending => "pending".to_string(),
            StartupContextApplyTargetState::Unchanged => "unchanged".to_string(),
            StartupContextApplyTargetState::Applied {
                revision: Some(revision),
            } => {
                format!("applied · revision {revision}")
            }
            StartupContextApplyTargetState::Applied { revision: None } => "applied".to_string(),
            StartupContextApplyTargetState::Failed { message, retryable } => format!(
                "failed{} · {message}",
                if *retryable { " · retryable" } else { "" }
            ),
            StartupContextApplyTargetState::Canceled => "canceled".to_string(),
        }
    }
}
