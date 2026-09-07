//! Unsaved local intent and correlated manager mutation replies.
pub(crate) mod export;
pub(crate) mod external;
mod forms;
pub(crate) mod local_recovery;
mod render;
#[cfg(test)]
mod tests;
use super::*;
use forms::EditForm;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum EditAction {
    ImportLegacy,
    RestoreHead,
    ExportRevision,
    CopyGlobal,
    CopyProject,
    Recoveries,
    RetryLocalStorage,
    LocalRecoveries,
    ReviewLocalValues,
    CompareCurrent,
    ReconcileProposed,
    EditCurrent,
    RetrySave,
    GlobalRepository,
    ProjectRepository,
    ApplyRepository,
    RepositoryReceipt,
    Open,
    CreateGlobal,
    CreateProject,
    Redefine,
    Addendum,
    Rename,
    Clear,
    Delete,
    Restore,
    CommitExternal,
    GlobalSettings,
    ProjectSettings,
    Body,
    Metadata,
    Review,
    Save,
    Close,
    Discard,
    NextFile,
    PreviousFile,
    Diff,
    Preview,
}

#[derive(Clone, Debug)]
pub(crate) struct EditorRequest {
    pub draft: String,
    pub generation: u64,
    pub file: String,
    pub body: String,
    pub repair: bool,
    pub metadata_field: Option<usize>,
}

#[derive(Default)]
pub(crate) struct EditingUi {
    pub export: Option<InstructionRevisionExport>,
    pub local_loading: bool,
    pub visible: bool,
    pub recovery_dirty: bool,
    pub storage_blocked: bool,
    pub request_preserved: bool,
    pub suspended_request: Option<InstructionManagementRequest>,
    pub resume_needed: bool,
    pub local_request: Option<local_recovery::LocalRecoveryRequest>,
    pub archiving: bool,
    pub local_loaded: Option<local_recovery::LocalSnapshot>,
    local_review_anchor: Option<forms::FormAnchor>,
    pub draft: Option<InstructionEditDraft>,
    pub queued: Option<InstructionManagementRequest>,
    pub pending: Option<(u64, String, InstructionManagementRequest)>,
    pub review: Option<InstructionEditReview>,
    pub editor: Option<EditorRequest>,
    pub status: String,
    pub file_index: usize,
    pub scroll: usize,
    pub document: String,
    pub wrapped: Vec<String>,
    pub wrap_width: u16,
    form: Option<EditForm>,
    submitted_form: Option<EditForm>,
    pub hits: Vec<(Rect, EditAction)>,
    pub field_hits: Vec<(Rect, usize)>,
    pub confirm: Option<EditAction>,
    confirm_anchor: Option<forms::FormAnchor>,
    pub failed: bool,
    pub recovery_id: Option<String>,
    pub repository_plan: Option<InstructionRepositoryPlan>,
    pub repository_receipt: Option<InstructionRepositoryReceipt>,
    pub recoveries: Option<InstructionRecoveryList>,
    pub conflict: Option<InstructionDraftConflict>,
}
impl EditingUi {
    pub(crate) fn set_external_metadata_value(&mut self, index: usize, value: String) {
        if let Some(form) = &mut self.form {
            form.set_external_value(index, value);
        }
        self.status="Complete metadata value retained in the unsaved form. Submit it, then Review before Save.".into();
        self.recovery_dirty = true;
    }
    fn busy(&self) -> bool {
        self.local_loading
            || self.archiving
            || self.queued.is_some()
            || self.pending.is_some()
            || self.editor.is_some()
    }
    pub fn reserve(&mut self, id: u64, session: &str) -> Option<InstructionManagementRequest> {
        let request = self.queued.take()?;
        self.request_preserved = false;
        self.pending = Some((id, session.into(), request.clone()));
        self.failed = false;
        self.status = match &request {
            InstructionManagementRequest::Save { .. } => "Saving reviewed instructions. Closing or losing the connection does not undo a published commit.".into(),
            InstructionManagementRequest::Review { .. } => "Validating complete draft and affected consumers…".into(),
            _ => "Preparing instruction edit…".into(),
        };
        Some(request)
    }
    pub fn accept(&mut self, id: u64, reply: InstructionManagementReply) -> bool {
        let Some((expected_id, session, request)) = &self.pending else {
            return false;
        };
        if id != *expected_id || &reply.session_id != session {
            return false;
        }
        let matches_draft = |draft: &InstructionEditDraft| match request {
            InstructionManagementRequest::Begin { .. } => true,
            InstructionManagementRequest::ReconcileDraft { draft: old, .. } => draft.id != *old,
            InstructionManagementRequest::Update {
                draft: expected,
                generation,
                ..
            } => &draft.id == expected && draft.generation == generation.saturating_add(1),
            InstructionManagementRequest::Resume {
                draft: expected, ..
            } => &draft.id == expected,
            _ => false,
        };
        match &reply.result {
            InstructionManagementResult::FileSaved { draft, .. } if !matches!(request,InstructionManagementRequest::Save{draft:expected,..} if draft==expected) =>
            {
                return false;
            }
            InstructionManagementResult::RevisionExport(export) if !matches!(request, InstructionManagementRequest::ExportRevision { revision, .. } if revision == &export.revision) =>
            {
                return false;
            }
            InstructionManagementResult::Recoveries(_)
                if !matches!(request, InstructionManagementRequest::Recoveries) =>
            {
                return false;
            }
            InstructionManagementResult::DraftConflict(conflict) if !matches!(request, InstructionManagementRequest::CompareDraft { draft, generation } if draft == &conflict.draft && generation == &conflict.generation) =>
            {
                return false;
            }

            InstructionManagementResult::RepositoryChoices(choices) if !matches!(request, InstructionManagementRequest::RepositoryChoices { scope } if scope == &choices.scope) =>
            {
                return false;
            }
            InstructionManagementResult::RepositoryPlan(plan) if !matches!(request, InstructionManagementRequest::PlanRepository { scope, action } if scope == &plan.scope && std::mem::discriminant(action) == std::mem::discriminant(&plan.action)) =>
            {
                return false;
            }
            InstructionManagementResult::RepositoryReceipt(receipt) if !matches!(request, InstructionManagementRequest::ApplyRepository { operation_id } | InstructionManagementRequest::RepositoryReceipt { operation_id } if operation_id == &receipt.id) =>
            {
                return false;
            }

            InstructionManagementResult::Draft(draft) if !matches_draft(draft) => return false,
            InstructionManagementResult::Reviewed(review) if !matches!(request, InstructionManagementRequest::Review { draft, generation } if draft == &review.draft.id && generation == &review.draft.generation) =>
            {
                return false;
            }
            InstructionManagementResult::Saved { draft: actual, .. } if !matches!(request, InstructionManagementRequest::Save { draft, .. } if draft == actual) =>
            {
                return false;
            }
            InstructionManagementResult::Closed
                if !matches!(request, InstructionManagementRequest::Close) =>
            {
                return false;
            }
            InstructionManagementResult::Discarded
                if !matches!(request, InstructionManagementRequest::Discard { .. }) =>
            {
                return false;
            }
            _ => {}
        }
        self.pending = None;
        self.recovery_dirty = true;
        self.wrapped.clear();
        match reply.result {
            InstructionManagementResult::FileSaved {
                draft,
                path,
                no_change,
            } => {
                self.status = format!(
                    "{} working file {path}. No parent staging or commit occurred.",
                    if no_change { "Unchanged" } else { "Saved" }
                );
                self.document = self.status.clone();
                self.review = None;
                self.suspended_request = None;
                self.local_loaded = None;
                if let Some(current) = &mut self.draft
                    && current.id == draft
                {
                    current.save_started = true;
                    current.committed = Some("working file saved, not committed".into());
                }
            }

            InstructionManagementResult::RevisionExport(export) => {
                self.local_loading = true;
                self.export = Some(export);
                self.status =
                    "Writing complete revision export to a new client-local directory…".into();
            }
            InstructionManagementResult::Recoveries(list) => {
                self.status = "Choose retained unsaved work or inspect an operation receipt. No source action runs automatically.".into();
                self.recoveries = Some(list);
            }
            InstructionManagementResult::DraftConflict(conflict) => {
                self.document = format!(
                    "STALE DRAFT COMPARISON\nCurrent HEAD: {}\nBranch: {}\n\nNothing was overwritten. Choose Edit current source to start from the external version, or Keep proposed text to retain your proposed text against this compared base. Both create a new private draft, preserve the original, and require another reviewed Save. No automatic text merge occurs.\n",
                    conflict.head,
                    conflict.branch.as_deref().unwrap_or("detached")
                );
                for file in &conflict.files {
                    self.document.push_str(&format!(
                        "\nExecutable modes: base {}, working {}, proposed {}\n",
                        file.base_executable, file.working_executable, file.proposed_executable
                    ));
                    self.document.push_str(&format!("\nFILE {}\nOPENED BASE\n{}\nCURRENT WORKING SOURCE\n{}\nYOUR PROPOSED VERSION\n{}\n", file.path, file.base.as_deref().unwrap_or("(absent)"), file.working.as_deref().unwrap_or("(absent)"), file.proposed.as_deref().unwrap_or("(delete)")));
                }
                self.conflict = Some(conflict);
                self.scroll = 0;
                self.visible = true;
                self.status = "Complete original, current and proposed versions. Actions offers explicit recovery choices.".into();
            }

            InstructionManagementResult::RepositoryChoices(choices) => {
                self.visible = true;
                self.form = Some(EditForm::repository(choices));
                self.status =
                    "Choose an explicit repository operation, then review its exact consequences."
                        .into();
            }
            InstructionManagementResult::RepositoryPlan(plan) => {
                self.visible = true;
                self.document = format!(
                    "{}\n\nNetwork action: {}\nOperation: {}\n\nOUTGOING COMMITS\n{}",
                    plan.detail,
                    plan.network,
                    plan.id,
                    plan.outgoing_commits.join("\n")
                );
                self.repository_plan = Some(plan);
                self.repository_receipt = None;
                self.confirm = Some(EditAction::ApplyRepository);
                self.scroll = 0;
                self.status = "Reviewed repository action. Y applies once; N returns to the form without changing source.".into();
            }
            InstructionManagementResult::RepositoryReceipt(receipt) => {
                self.visible = true;
                self.confirm = None;
                self.document = format!(
                    "{}\n\n{}\n\nOperation: {}\nCompleted: {}\nOutcome uncertain: {}",
                    receipt.title,
                    receipt.detail,
                    receipt.id,
                    receipt.completed,
                    receipt.outcome_uncertain
                );
                self.failed = !receipt.completed && !receipt.running;
                self.status = if receipt.completed {
                    "Repository action completed. No automatic follow-up or current-session activation.".into()
                } else if receipt.running {
                    "Repository operation is still running. Read its receipt again when ready."
                        .into()
                } else {
                    "Repository action needs attention. Inspect the retained outcome before any further explicit action.".into()
                };
                if receipt.completed {
                    self.submitted_form = None;
                }
                self.repository_receipt = Some(receipt);
                self.scroll = 0;
            }

            InstructionManagementResult::Draft(draft) => {
                self.submitted_form = None;
                self.conflict = None;
                self.visible = true;
                self.review = None;
                self.document.clear();
                self.scroll = 0;
                if self.draft.as_ref().is_none_or(|old| old.id != draft.id) {
                    self.file_index = draft.files.iter().position(|file| matches!(&file.metadata, InstructionEditMetadata::Resource(fields) if fields.kind == InstructionEditKind::Skill)).unwrap_or(0);
                } else {
                    self.file_index = self.file_index.min(draft.files.len().saturating_sub(1));
                }
                self.recovery_id = Some(draft.id.clone());
                let committed = draft.committed.clone();
                self.draft = Some(draft);
                self.status = "Unsaved draft. Edit body or metadata, then Review changes. Current instructions are unchanged.".into();
                if let Some(commit) = committed {
                    self.status = format!(
                        "This draft already completed at {commit}. Source was not replayed. Close it and open a new edit for further changes."
                    );
                }
            }
            InstructionManagementResult::Reviewed(review) => {
                self.status = if review.errors.is_empty() {
                    "Validated draft. Review the complete diff, then Save once to commit locally."
                        .into()
                } else {
                    "Validation failed. Source is unchanged and the draft is preserved. Repair the listed files, then Review again.".into()
                };
                self.failed = !review.errors.is_empty();
                self.draft = Some(review.draft.clone());
                self.review = Some(review);
                self.show_diff();
            }
            InstructionManagementResult::Saved {
                commit,
                no_change,
                recovered,
                paths,
                ..
            } => {
                self.status = format!(
                    "{} at {}. {} owned path(s). Nothing pushed or committed in the parent project. Current session instructions are unchanged.",
                    if no_change {
                        "No change"
                    } else if recovered {
                        "Recovered completed Save"
                    } else {
                        "Committed"
                    },
                    commit,
                    paths.len()
                );
                self.suspended_request = None;
                self.local_loaded = None;
                self.document = self.status.clone();
                self.review = None;
                if let Some(draft) = &mut self.draft {
                    draft.save_started = true;
                    draft.committed = Some(commit.clone());
                    draft.reviewed = false;
                }
            }
            InstructionManagementResult::Closed | InstructionManagementResult::Discarded => {
                self.suspended_request = None;
                self.local_loaded = None;
                self.resume_needed = false;
                self.conflict = None;
                self.draft = None;
                self.review = None;
                self.visible = false;
                self.form = None;
                self.document.clear();
                self.status = "Draft closed. No automatic Save.".into();
            }
            InstructionManagementResult::Failed(error) => {
                self.failed = true;
                if let Some(mut form) = self.submitted_form.take() {
                    form.error = error.detail.clone();
                    self.form = Some(form);
                }
                self.recovery_id = error
                    .draft
                    .or_else(|| self.draft.as_ref().map(|draft| draft.id.clone()));
                self.status = format!(
                    "{}: {}\n{}{}",
                    error.operation,
                    error.detail,
                    if error.source_unchanged {
                        "Authoritative source was not changed."
                    } else {
                        "The operation may have completed some writes. Inspect source and its receipt before retrying."
                    },
                    self.recovery_id
                        .as_ref()
                        .map(|id| format!(" Draft retained: {id}"))
                        .unwrap_or_default()
                );
                self.document = self.status.clone();
                self.scroll = 0;
            }
        }
        true
    }
    fn show_diff(&mut self) {
        self.document.clear();
        self.scroll = 0;
        let Some(review) = &self.review else { return };
        self.document.push_str(&format!(
            "{}\n{}\nRepository/source: {}\nBranch: {}\n\n",
            if review.draft.working_file_only {
                "REVIEWED WORKING-FILE SAVE (NO COMMIT)"
            } else {
                "REVIEWED LOCAL COMMIT"
            },
            review.draft.subject,
            review.draft.repository,
            review
                .draft
                .branch
                .as_deref()
                .unwrap_or(if review.draft.working_file_only {
                    "Not applicable: parent remains uncommitted"
                } else {
                    "DETACHED: Save unavailable"
                })
        ));
        for warning in &review.draft.warnings {
            self.document.push_str(&format!("{warning}\n"));
        }
        for error in &review.errors {
            self.document.push_str(&format!("ERROR: {error}\n"));
        }
        for file in &review.files {
            if file.committed_executable != file.proposed_executable {
                self.document.push_str(&format!(
                    "\n{}: executable mode {} -> {}\n",
                    file.path, file.committed_executable, file.proposed_executable
                ));
            }
            self.document.push_str(&format!("\nFILE {}\n", file.path));
            let before = file.committed.as_deref().unwrap_or_default();
            let after = file.proposed.as_deref().unwrap_or_default();
            let diff = similar::TextDiff::from_lines(before, after);
            self.document.push_str(
                &diff
                    .unified_diff()
                    .header(
                        &format!(
                            "{}/{}",
                            if review.draft.working_file_only {
                                "opened"
                            } else {
                                "HEAD"
                            },
                            file.path
                        ),
                        &format!("draft/{}", file.path),
                    )
                    .to_string(),
            );
            if file.working != file.committed {
                self.document
                    .push_str("\nEXTERNAL WORKING CHANGES: draft versus current working file\n");
                self.document.push_str(
                    &similar::TextDiff::from_lines(
                        file.working.as_deref().unwrap_or_default(),
                        after,
                    )
                    .unified_diff()
                    .header("working", "draft")
                    .to_string(),
                );
            }
        }
    }
    fn show_previews(&mut self) {
        self.document = self
            .review
            .as_ref()
            .map(|review| {
                review
                    .previews
                    .iter()
                    .map(|preview| format!("{}\n\n{}", preview.title, preview.content))
                    .collect::<Vec<_>>()
                    .join("\n\n")
            })
            .unwrap_or_else(|| "Review the draft to render affected previews.".into());
        self.scroll = 0;
    }
}

impl InstructionManager {
    pub(super) fn edit_action(&mut self, action: EditAction) {
        if action == EditAction::RetryLocalStorage {
            self.editing.storage_blocked = false;
            if !self.editing.visible
                && self.rows_loading()
                && matches!(
                    action,
                    EditAction::Open
                        | EditAction::Clear
                        | EditAction::Delete
                        | EditAction::Restore
                        | EditAction::RestoreHead
                        | EditAction::ImportLegacy
                        | EditAction::Redefine
                        | EditAction::CopyGlobal
                        | EditAction::CopyProject
                )
            {
                self.status =
                    "Wait for the selected source to finish loading before editing it.".into();
                return;
            }
            self.editing.recovery_dirty = true;
            self.editing.status =
                "Retrying local recovery persistence before any pending source action.".into();
            return;
        }
        if self.editing.storage_blocked {
            self.editing.status = "Local recovery storage failed. Repair it and choose Retry local recovery storage. No pending source action was dispatched.".into();
            return;
        }
        self.editing.recovery_dirty = true;
        if self.render_only {
            self.status = "Render-only fixture: no source operation is sent.".into();
            return;
        }
        if self.editing.resume_needed
            && matches!(
                action,
                EditAction::Body
                    | EditAction::Metadata
                    | EditAction::Save
                    | EditAction::Review
                    | EditAction::RetrySave
            )
        {
            self.editing.status =
                "Recover the server draft first; local values are preserved.".into();
            return;
        }
        if self.editing.busy() {
            self.editing.status =
                "Wait for the current edit operation. Its draft and receipt are preserved.".into();
            return;
        }
        self.editing.wrapped.clear();
        match action {
            EditAction::ImportLegacy => self.begin_edit(InstructionEditAction::ImportLegacy),
            EditAction::RestoreHead => {
                if let (Some(snapshot), Some(target)) = (self.snapshot_id(), self.selected_target())
                {
                    self.editing.queued = Some(InstructionManagementRequest::Begin {
                        snapshot,
                        target,
                        action: InstructionEditAction::Restore {
                            revision: "HEAD".into(),
                        },
                    });
                    self.editing.visible = true;
                }
            }
            EditAction::ExportRevision => self.history_resource_action(true),

            EditAction::CopyGlobal | EditAction::CopyProject => {
                let mut form = EditForm::start(action, None);
                form.anchor = Some(self.form_anchor());
                self.editing.form = Some(form);
                self.editing.visible = true;
            }
            EditAction::RetryLocalStorage => {}
            EditAction::LocalRecoveries => {
                self.editing.local_loading = true;
                self.editing.local_request = Some(local_recovery::LocalRecoveryRequest::List);
            }
            EditAction::ReviewLocalValues => self.review_local_values(),
            EditAction::Recoveries => {
                self.editing.queued = Some(InstructionManagementRequest::Recoveries);
            }
            EditAction::CompareCurrent => {
                if let Some(draft) = &self.editing.draft {
                    self.editing.queued = Some(InstructionManagementRequest::CompareDraft {
                        draft: draft.id.clone(),
                        generation: draft.generation,
                    });
                }
            }
            EditAction::ReconcileProposed | EditAction::EditCurrent => {
                if self.editing.conflict.is_some() {
                    self.editing.confirm = Some(action);
                    self.editing.status = "Create a new private draft using the compared state? Y confirms. Original draft and source remain unchanged.".into();
                }
            }
            EditAction::RetrySave => {
                if let Some(draft) = &self.editing.draft {
                    self.editing.queued = Some(InstructionManagementRequest::Save {
                        draft: draft.id.clone(),
                        generation: draft.generation,
                    });
                }
            }

            EditAction::GlobalRepository | EditAction::ProjectRepository => {
                self.editing.visible = true;
                self.editing.queued = Some(InstructionManagementRequest::RepositoryChoices {
                    scope: if action == EditAction::GlobalRepository {
                        InstructionEditScope::Global
                    } else {
                        InstructionEditScope::Project
                    },
                });
            }
            EditAction::RepositoryReceipt => {
                if let Some(plan) = &self.editing.repository_plan {
                    self.editing.queued = Some(InstructionManagementRequest::RepositoryReceipt {
                        operation_id: plan.id.clone(),
                    });
                    self.editing.visible = true;
                }
            }
            EditAction::ApplyRepository => {
                if let Some(plan) = &self.editing.repository_plan {
                    self.editing.queued = Some(InstructionManagementRequest::ApplyRepository {
                        operation_id: plan.id.clone(),
                    });
                }
            }

            EditAction::CreateGlobal
            | EditAction::CreateProject
            | EditAction::Rename
            | EditAction::Addendum => {
                self.editing.visible = true;
                let mut form = EditForm::start(action, self.editing.draft.as_ref());
                form.anchor = Some(self.form_anchor());
                self.editing.form = Some(form);
            }
            EditAction::Open
            | EditAction::Redefine
            | EditAction::CommitExternal
            | EditAction::GlobalSettings
            | EditAction::ProjectSettings => {
                let operation = match action {
                    EditAction::Redefine => InstructionEditAction::RedefineInProject,
                    EditAction::CommitExternal => InstructionEditAction::CommitExternal,
                    EditAction::GlobalSettings => InstructionEditAction::Settings {
                        scope: InstructionEditScope::Global,
                    },
                    EditAction::ProjectSettings => InstructionEditAction::Settings {
                        scope: InstructionEditScope::Project,
                    },
                    _ => InstructionEditAction::Edit,
                };
                self.begin_edit(operation);
            }
            EditAction::Clear | EditAction::Delete | EditAction::Restore | EditAction::Discard => {
                self.editing.visible = true;
                self.editing.confirm_anchor = Some(self.form_anchor());
                self.editing.confirm = Some(action);
                self.editing.document = match action { EditAction::Clear => "Clear the selected body? Identity remains, and empty project instructions suppress global prose. You will review the diff before committing.", EditAction::Delete => "Delete this user resource? Project deletion reveals global guidance. Referencing files must be repaired in the same reviewed commit. Nothing is written until Save.", EditAction::Restore => "Restore this selected Git revision through a new reviewed commit? Other files and parent history remain untouched.", _ => "Discard the unsaved draft? This removes its recovery record. It does not undo a completed Save or external working changes." }.into();
                self.editing.scroll = 0;
            }
            EditAction::Metadata => {
                if let Some(draft) = &self.editing.draft
                    && let Some(file) = draft.files.get(self.editing.file_index)
                    && !file.deleted
                {
                    let mut form = EditForm::metadata(file, &draft.choices);
                    form.anchor = Some(self.form_anchor());
                    self.editing.form = Some(form);
                }
            }
            EditAction::Body => {
                if let Some(draft) = &self.editing.draft
                    && let Some(file) = draft.files.get(self.editing.file_index)
                    && !file.deleted
                {
                    match &file.metadata {
                        InstructionEditMetadata::Resource(_)
                        | InstructionEditMetadata::Damaged { .. }
                        | InstructionEditMetadata::Ecosystem => {
                            self.editing.editor = Some(EditorRequest {
                                draft: draft.id.clone(),
                                generation: draft.generation,
                                file: file.key.clone(),
                                body: file.body.clone(),
                                metadata_field: None,
                                repair: matches!(
                                    file.metadata,
                                    InstructionEditMetadata::Damaged { .. }
                                ),
                            })
                        }
                        _ => self.editing.status =
                            "Use typed metadata fields for repository settings and model aliases."
                                .into(),
                    }
                }
            }
            EditAction::Review | EditAction::Save => {
                if let Some(draft) = &self.editing.draft {
                    if action == EditAction::Save
                        && (!draft.reviewed || (draft.branch.is_none() && !draft.working_file_only))
                    {
                        self.editing.status =
                            "Save needs a validated reviewed draft on an attached branch.".into();
                        return;
                    }
                    self.editing.queued = Some(if action == EditAction::Review {
                        InstructionManagementRequest::Review {
                            draft: draft.id.clone(),
                            generation: draft.generation,
                        }
                    } else {
                        InstructionManagementRequest::Save {
                            draft: draft.id.clone(),
                            generation: draft.generation,
                        }
                    });
                }
            }
            EditAction::Close => {
                if self.editing.suspended_request.is_some() || self.editing.local_loaded.is_some() {
                    self.editing.archiving = true;
                    self.editing.local_request =
                        Some(local_recovery::LocalRecoveryRequest::Archive);
                    self.editing.status = "Preserving unsent local intent before closing…".into();
                    return;
                }
                if self.editing.failed
                    && self.editing.submitted_form.is_some()
                    && self.editing.draft.is_none()
                {
                    self.editing.form = self.editing.submitted_form.take();
                    self.editing.document.clear();
                    self.editing.wrapped.clear();
                    return;
                }
                self.editing.queued = self
                    .editing
                    .draft
                    .as_ref()
                    .map(|_| InstructionManagementRequest::Close);
                if self.editing.draft.is_none() {
                    self.editing.visible = false;
                    self.editing.form = None;
                }
            }
            EditAction::NextFile | EditAction::PreviousFile => {
                let count = self
                    .editing
                    .draft
                    .as_ref()
                    .map_or(0, |draft| draft.files.len());
                if count > 0 {
                    self.editing.file_index = if action == EditAction::NextFile {
                        (self.editing.file_index + 1) % count
                    } else {
                        (self.editing.file_index + count - 1) % count
                    };
                    self.editing.document.clear();
                    self.editing.scroll = 0;
                }
            }
            EditAction::Diff => self.editing.show_diff(),
            EditAction::Preview => self.editing.show_previews(),
        }
    }
    fn begin_edit(&mut self, action: InstructionEditAction) {
        let Some(snapshot) = &self.snapshot else {
            self.status = "Refresh inspection before editing.".into();
            return;
        };
        let target = self
            .selected_target()
            .unwrap_or(InstructionInspectionTarget::Session);
        self.editing.visible = true;
        self.editing.queued = Some(InstructionManagementRequest::Begin {
            snapshot: snapshot.snapshot.clone(),
            target,
            action,
        });
    }
    pub(super) fn edit_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        self.editing.recovery_dirty |= self.editing.visible;
        if !self.editing.visible {
            return false;
        }
        if self.editing.form.is_some() {
            self.edit_form_key(code, modifiers);
            return true;
        }
        if let Some(action) = self.editing.confirm {
            match code {
                KeyCode::Down | KeyCode::PageDown => {
                    self.editing.scroll =
                        self.editing
                            .scroll
                            .saturating_add(if code == KeyCode::PageDown {
                                self.detail_height.max(1)
                            } else {
                                1
                            });
                    return true;
                }
                KeyCode::Up | KeyCode::PageUp => {
                    self.editing.scroll =
                        self.editing
                            .scroll
                            .saturating_sub(if code == KeyCode::PageUp {
                                self.detail_height.max(1)
                            } else {
                                1
                            });
                    return true;
                }
                _ => {}
            }
            match code {
                KeyCode::Char('y' | 'Y') => {
                    if matches!(
                        action,
                        EditAction::Clear
                            | EditAction::Delete
                            | EditAction::Restore
                            | EditAction::Discard
                    ) && self.editing.confirm_anchor.as_ref() != Some(&self.form_anchor())
                    {
                        self.editing.confirm = None;
                        self.editing.status="Selection, revision or draft changed during confirmation. Nothing was applied; choose the action again.".into();
                        return true;
                    }
                    self.editing.confirm = None;
                    self.editing.document.clear();
                    match action {
                        EditAction::ReviewLocalValues => self.apply_local_values(),
                        EditAction::ReconcileProposed | EditAction::EditCurrent => {
                            if let Some(conflict) = &self.editing.conflict {
                                self.editing.queued =
                                    Some(InstructionManagementRequest::ReconcileDraft {
                                        draft: conflict.draft.clone(),
                                        generation: conflict.generation,
                                        comparison: conflict.comparison.clone(),
                                        use_working_content: action == EditAction::EditCurrent,
                                    });
                            }
                        }

                        EditAction::ApplyRepository => {
                            self.edit_action(EditAction::ApplyRepository)
                        }
                        EditAction::Clear => self.begin_edit(InstructionEditAction::Clear),
                        EditAction::Delete => self.begin_edit(InstructionEditAction::Delete),
                        EditAction::Restore => self.history_resource_action(false),
                        EditAction::Discard => {
                            if let Some(draft) = &self.editing.draft {
                                self.editing.queued = Some(InstructionManagementRequest::Discard {
                                    draft: draft.id.clone(),
                                    generation: draft.generation,
                                });
                            }
                        }
                        _ => {}
                    }
                }
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('n' | 'N') => {
                    if matches!(
                        action,
                        EditAction::ApplyRepository | EditAction::ReviewLocalValues
                    ) {
                        self.editing.form = self.editing.submitted_form.take();
                        self.editing.confirm = None;
                        self.editing.document.clear();
                        self.editing.wrapped.clear();
                        return true;
                    }
                    self.editing.confirm = None;
                    self.editing.document.clear();
                    if self.editing.draft.is_none() {
                        self.editing.visible = false;
                    }
                }
                _ => {}
            }
            return true;
        }
        match code {
            KeyCode::Esc => self.edit_action(EditAction::Close),
            KeyCode::Char('q' | 'Q') => {
                self.edit_action(EditAction::Close);
                self.visible = false;
                self.queued = Some(InstructionInspectionRequest::Close);
            }
            KeyCode::Char(' ' | ':' | '?') => self.open_actions(false),
            KeyCode::Char('b') => self.edit_action(EditAction::Body),
            KeyCode::Char('m') => self.edit_action(EditAction::Metadata),
            KeyCode::Char('r') => self.edit_action(EditAction::Review),
            KeyCode::Char('s') => self.edit_action(EditAction::Save),
            KeyCode::Char('d') => self.edit_action(EditAction::Diff),
            KeyCode::Char('v') => self.edit_action(EditAction::Preview),
            KeyCode::Tab => self.edit_action(EditAction::NextFile),
            KeyCode::BackTab => self.edit_action(EditAction::PreviousFile),
            KeyCode::Down | KeyCode::Char('j') => {
                self.editing.scroll = self.editing.scroll.saturating_add(1)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.editing.scroll = self.editing.scroll.saturating_sub(1)
            }
            KeyCode::PageDown => {
                self.editing.scroll = self
                    .editing
                    .scroll
                    .saturating_add(self.detail_height.max(1))
            }
            KeyCode::PageUp => {
                self.editing.scroll = self
                    .editing
                    .scroll
                    .saturating_sub(self.detail_height.max(1))
            }
            KeyCode::Home => self.editing.scroll = 0,
            KeyCode::End => self.editing.scroll = usize::MAX,
            _ => {}
        }
        true
    }
    pub(super) fn edit_paste(&mut self, text: &str) -> bool {
        self.editing.recovery_dirty |= self.editing.visible;
        if !self.editing.visible {
            return false;
        }
        if let Some(form) = &mut self.editing.form {
            form.paste(text);
        } else {
            self.editing.status =
                "Use Edit body to paste prose into the external editor, or open a metadata field."
                    .into();
        }
        true
    }
}

impl InstructionManager {
    pub(super) fn edit_menu_items(&self) -> Vec<super::menu::MenuItem> {
        use super::menu::{MenuAction, MenuItem};
        let draft = self.editing.draft.as_ref();
        let row = self.selected_target().and_then(|target| match target {
            InstructionInspectionTarget::Resource(key) => self
                .rows
                .iter()
                .find(|row| row.key == key)
                .or(self.detail_row.as_ref().filter(|row| row.key == key)),
            _ => None,
        });
        let entries: Vec<(EditAction, &str, &str, &str)> = if self.editing.visible {
            vec![
                (
                    EditAction::Body,
                    "Edit body",
                    "Open a complete private local draft in VISUAL, EDITOR or nano",
                    "B",
                ),
                (
                    EditAction::Metadata,
                    "Edit metadata",
                    "Typed resource fields, model aliases or default-agent settings",
                    "M",
                ),
                (
                    EditAction::Review,
                    "Review changes",
                    "Validate the complete graph, inspect exact diffs and affected previews",
                    "R",
                ),
                (
                    EditAction::Save,
                    "Save reviewed changes",
                    "One scoped local instruction commit, no parent commit or push",
                    "S",
                ),
                (
                    EditAction::Diff,
                    "Read complete diff",
                    "Committed and working source compared with the proposed version",
                    "D",
                ),
                (
                    EditAction::Preview,
                    "Read affected previews",
                    "Synthetic typed previews, not current session instructions",
                    "V",
                ),
                (
                    EditAction::NextFile,
                    "Next affected file",
                    "Select another file in this atomic reference-repair draft",
                    "Tab",
                ),
                (
                    EditAction::PreviousFile,
                    "Previous affected file",
                    "Return to the previous draft file",
                    "Shift-Tab",
                ),
                (
                    EditAction::Close,
                    "Close and keep draft",
                    "Release editing ownership and preserve unsaved recovery data",
                    "Esc",
                ),
                (
                    EditAction::Discard,
                    "Discard unsaved draft",
                    "Explicit confirmation removes the draft recovery record, not Git history",
                    "choose",
                ),
            ]
        } else {
            vec![
                (
                    EditAction::GlobalRepository,
                    "Global repository controls",
                    "Review initialization, recovery, branch and explicit synchronization actions",
                    "choose",
                ),
                (
                    EditAction::ProjectRepository,
                    "Project repository controls",
                    "Set up, repair, attach or synchronize this project's instruction repository",
                    "choose",
                ),
                (
                    EditAction::RepositoryReceipt,
                    "Recover last repository outcome",
                    "Read a durable receipt without replaying an uncertain operation",
                    "choose",
                ),
                (
                    EditAction::Open,
                    "Edit instruction",
                    "Draft working source without changing it until reviewed Save",
                    "Ctrl-E",
                ),
                (
                    EditAction::CreateGlobal,
                    "Create global resource",
                    "Choose typed identity and metadata, then edit its body",
                    "choose",
                ),
                (
                    EditAction::CreateProject,
                    "Create project resource",
                    "Create in the configured project instruction repository",
                    "choose",
                ),
                (
                    EditAction::Redefine,
                    "Redefine in project",
                    "Copy this global definition into project scope, with a consequence warning",
                    "choose",
                ),
                (
                    EditAction::Addendum,
                    "Create agent addendum",
                    "Create explicit project guidance targeting the selected agent",
                    "choose",
                ),
                (
                    EditAction::Rename,
                    "Rename with reference repair",
                    "Repair this repository atomically; cross-repository changes stay explicit",
                    "choose",
                ),
                (
                    EditAction::Clear,
                    "Clear body",
                    "Keep identity and intentionally empty the body after diff review",
                    "choose",
                ),
                (
                    EditAction::Delete,
                    "Delete with reference repair",
                    "Remove a user resource only after repairing its references",
                    "choose",
                ),
                (
                    EditAction::Restore,
                    "Restore revision",
                    "Restore the selected historical content as a new commit",
                    "choose",
                ),
                (
                    EditAction::CommitExternal,
                    "Review external changes",
                    "Validate and commit the current working version, or edit on top",
                    "choose",
                ),
                (
                    EditAction::GlobalSettings,
                    "Global default agent",
                    "Change the global store's typed default-agent setting",
                    "choose",
                ),
                (
                    EditAction::ProjectSettings,
                    "Project default agent",
                    "Change the configured project's typed default-agent setting",
                    "choose",
                ),
            ]
        };
        let mut entries = entries;
        if self.editing.visible && draft.is_none() {
            entries = vec![
                (
                    EditAction::GlobalRepository,
                    "Global repository controls",
                    "Choose another explicit repository action",
                    "choose",
                ),
                (
                    EditAction::ProjectRepository,
                    "Project repository controls",
                    "Configure or synchronize this project's instructions",
                    "choose",
                ),
                (
                    EditAction::RepositoryReceipt,
                    "Read last repository receipt",
                    "Recover an outcome without repeating the operation",
                    "choose",
                ),
                (
                    EditAction::Close,
                    "Back to instructions",
                    "Return to source inspection",
                    "Esc",
                ),
            ];
        }
        if draft.is_some() {
            entries.extend([
                (EditAction::CompareCurrent, "Compare draft with current source", "Read complete opened, current and proposed versions without changing files", "choose"),
                (EditAction::ReconcileProposed, "Keep proposed text on compared base", "Create a new private draft; external changes are not automatically merged", "choose"),
                (EditAction::EditCurrent, "Edit current source instead", "Start a new private draft from the compared working version, preserving the original draft", "choose"),
                (EditAction::RetrySave, "Recover previous Save", "Retry the same durable Save identity, never a guessed new commit", "choose"),
            ]);
        } else if !entries
            .iter()
            .any(|(action, _, _, _)| *action == EditAction::Recoveries)
        {
            entries.push((
                EditAction::Recoveries,
                "Recover drafts and operations",
                "Find this session's retained drafts and receipts",
                "choose",
            ));
        }
        entries.push((
            EditAction::LocalRecoveries,
            "Recover local unsent changes",
            "Recover this client's retained form/editor intent without replaying source actions",
            "choose",
        ));
        if self.editing.local_loaded.is_some() || self.editing.suspended_request.is_some() {
            entries.push((EditAction::ReviewLocalValues, "Review retained local values", "Compare local intent with the current draft or inspected target before applying it", "choose"));
        }
        if self.editing.storage_blocked {
            entries.insert(0, (EditAction::RetryLocalStorage, "Retry local recovery storage", "After repairing storage, explicitly resume persistence before the pending action", "choose"));
        }
        if !self.editing.visible {
            entries.extend([
                (EditAction::CopyGlobal, "Copy skill to global", "Capture the complete selected external package into a reviewed managed global draft", "choose"),
                (EditAction::CopyProject, "Copy skill to project", "Capture the complete external package in this project's configured instruction store", "choose"),
            ]);
        }
        if !self.editing.visible {
            entries.extend([
                (
                    EditAction::ImportLegacy,
                    "Import legacy source",
                    "Review exact captured legacy source and its cutover receipt together",
                    "choose",
                ),
                (
                    EditAction::RestoreHead,
                    "Restore committed version",
                    "Review the selected resource at current Git HEAD; no history rewrite",
                    "choose",
                ),
                (
                    EditAction::ExportRevision,
                    "Export selected revision",
                    "Write complete historical file/package bytes to a new local export directory",
                    "choose",
                ),
            ]);
        }
        entries.into_iter().map(|(action, label, hint, key)| {
            let disabled = if action == EditAction::RetryLocalStorage { None } else if self.editing.busy() { Some("Wait for the current operation; its receipt is preserved.".into()) }
            else if matches!(action,EditAction::CompareCurrent|EditAction::ReconcileProposed|EditAction::EditCurrent) && draft.is_some_and(|draft|draft.working_file_only){Some("Review shows the current AGENTS.md working version. Close this preserved draft and edit the current source if it changed.".into())}
            else if matches!(action, EditAction::ReconcileProposed | EditAction::EditCurrent) && self.editing.conflict.is_none() { Some("Compare the draft with current source first.".into()) }
            else if action == EditAction::RetrySave && draft.is_none_or(|draft| !draft.save_started) { Some("No previous Save needs recovery.".into()) }
            else if action == EditAction::RepositoryReceipt && self.editing.repository_plan.is_none() { Some("No repository operation has been prepared in this manager.".into()) }
            else if self.editing.visible {
                if matches!(action, EditAction::LocalRecoveries | EditAction::ReviewLocalValues | EditAction::Recoveries | EditAction::Close | EditAction::GlobalRepository | EditAction::ProjectRepository | EditAction::RepositoryReceipt) { None }
                else if draft.is_none() { Some("No attached draft. Close this view or recover its retained draft.".into()) }
                else if matches!(action, EditAction::Body | EditAction::Metadata) && draft.and_then(|draft| draft.files.get(self.editing.file_index)).is_some_and(|file| matches!(file.metadata, InstructionEditMetadata::Binary { .. })) { Some("Binary package bytes are retained exactly. They are not editable as prompt text.".into()) }
                else if action == EditAction::Save && draft.is_some_and(|draft| !draft.reviewed || (draft.branch.is_none() && !draft.working_file_only)) { Some("Review this exact draft successfully on an attached branch before Save.".into()) }
                else if matches!(action, EditAction::Diff | EditAction::Preview) && self.editing.review.is_none() { Some("Review changes first.".into()) }
                else if draft.is_some_and(|draft| draft.save_started) && matches!(action, EditAction::Body | EditAction::Metadata) { Some("This Save has started or completed. Resolve its receipt, then open a new edit.".into()) }
                else { None }
            } else if self.snapshot.is_none() || self.rows_loading() { Some("Refresh source inspection before choosing an edit target.".into()) }
            else if matches!(action, EditAction::LocalRecoveries | EditAction::ReviewLocalValues | EditAction::Recoveries | EditAction::GlobalRepository | EditAction::ProjectRepository | EditAction::RepositoryReceipt | EditAction::CreateGlobal | EditAction::CreateProject | EditAction::GlobalSettings | EditAction::ProjectSettings) { None }
            else if matches!(action, EditAction::CopyGlobal | EditAction::CopyProject) {
                row.filter(|row| row.origin == InstructionOrigin::External && row.kind == "skill").is_none().then(|| "Select an external skill source, including an explicitly shadowed source.".into())
            }
            else if action == EditAction::ImportLegacy { row.filter(|row| row.origin == InstructionOrigin::Legacy).is_none().then(|| "Select a legacy compatibility source.".into()) }
            else if matches!(action, EditAction::Restore | EditAction::ExportRevision) && matches!(self.selected_target(), Some(InstructionInspectionTarget::Repository(_))) { if self.history_visible || self.revision_selection.is_some() { None } else { Some("Choose a revision in repository history first.".into()) } }
            else if action==EditAction::Open && row.is_some_and(|row|row.kind=="AGENTS.md"){None}
            else if row.is_none_or(|row| row.origin != InstructionOrigin::Managed) { Some("Select a managed resource. External skills use Copy; ecosystem files retain separate ownership.".into()) }
            else if action == EditAction::Redefine && row.is_some_and(|row| row.scope != "global" || matches!(row.kind.as_str(), "model-roster" | "store-settings")) { Some("Select a global instruction, not global-only model policy or store settings.".into()) }
            else if action == EditAction::Addendum && row.is_some_and(|row| row.kind != "agent") { Some("Select the agent that should receive the addendum.".into()) }
            else if action == EditAction::Restore && self.revision_selection.is_none() && !self.history_visible { Some("Select a revision in Git history first.".into()) }
            else { None };
            MenuItem { label: label.into(), hint: hint.into(), key: key.into(), action: MenuAction::Edit(action), disabled }
        }).collect()
    }
    pub(super) fn open_edit_menu(&mut self) {
        self.menu = Some(super::menu::Menu {
            title: "Draft actions".into(),
            items: self.edit_menu_items(),
            query: String::new(),
            selected: 0,
            context: self.selected_target(),
            snapshot: self.snapshot_id(),
            revision: None,
            parent: None,
            explanation: false,
            explanation_scroll: 0,
        });
    }
}

#[derive(Clone)]
pub(super) enum RecoveryChoice {
    Historical {
        snapshot: String,
        target: InstructionInspectionTarget,
        revision: String,
        path: String,
        export: bool,
    },
    Local(String),
    Draft(InstructionEditScope, String),
    Operation(String),
}
impl InstructionManager {
    pub(crate) fn accept_management(&mut self, id: u64, reply: InstructionManagementReply) -> bool {
        let closing_reply = matches!(
            &reply.result,
            InstructionManagementResult::Closed | InstructionManagementResult::Discarded
        );
        let acknowledged_update = matches!(&reply.result, InstructionManagementResult::Draft(_))
            && self
                .editing
                .pending
                .as_ref()
                .is_some_and(|(_, _, request)| {
                    matches!(request, InstructionManagementRequest::Update { .. })
                });
        let resumed = self
            .editing
            .pending
            .as_ref()
            .is_some_and(|(_, _, request)| {
                matches!(request, InstructionManagementRequest::Resume { .. })
            });
        let is_recovery = matches!(reply.result, InstructionManagementResult::Recoveries(_));
        if !self.editing.accept(id, reply) {
            return false;
        }
        if !self.visible && !closing_reply {
            // Q may hide the manager while its accepted operation is still
            // completing. Preserve that result, then detach the server draft
            // rather than keeping its lease behind a closed view.
            self.editing.queued = Some(InstructionManagementRequest::Close);
        }
        if acknowledged_update {
            self.editing.local_loaded = None;
            self.editing.suspended_request = None;
        }
        if is_recovery {
            self.open_recovery_menu();
        }
        if resumed && !self.editing.failed {
            self.complete_local_resume();
        }
        true
    }
    fn open_recovery_menu(&mut self) {
        use super::menu::{Menu, MenuAction, MenuItem};
        let Some(list) = &self.editing.recoveries else {
            return;
        };
        let mut items = Vec::new();
        for draft in &list.drafts {
            items.push(MenuItem { label: format!("{:?}: {} [{}]", draft.scope, draft.subject, draft.id.chars().take(8).collect::<String>()), hint: format!("Draft {} · generation {} · Save started: {} · Commit: {}. Resume does not Save or activate instructions.", draft.id, draft.generation, draft.save_started, draft.committed.as_deref().unwrap_or("none")), key: "Enter".into(), action: MenuAction::Recovery(RecoveryChoice::Draft(draft.scope, draft.id.clone())), disabled: self.editing.draft.as_ref().map(|_| "Close the current draft before resuming another.".into()) });
        }
        for operation in &list.operations {
            items.push(MenuItem { label: format!("Receipt: {} [{}]", operation.title, operation.id.chars().take(8).collect::<String>()), hint: format!("Started: {} · Completed: {}. Read the durable receipt without executing the operation.", operation.started, operation.completed), key: "Enter".into(), action: MenuAction::Recovery(RecoveryChoice::Operation(operation.id.clone())), disabled: None });
        }
        for error in &list.errors {
            items.push(MenuItem {
                label: "Recovery record needs inspection".into(),
                hint: error.clone(),
                key: "?".into(),
                action: MenuAction::Key(KeyCode::Esc),
                disabled: Some(error.clone()),
            });
        }
        if items.is_empty() {
            items.push(MenuItem {
                label: "No retained drafts or operations for this session".into(),
                hint: "Escape returns without changing source.".into(),
                key: "Esc".into(),
                action: MenuAction::Key(KeyCode::Esc),
                disabled: None,
            });
        }
        self.menu = Some(Menu {
            title: "Retained drafts and operations".into(),
            items,
            query: String::new(),
            selected: 0,
            context: self.selected_target(),
            snapshot: self.snapshot_id(),
            revision: None,
            parent: None,
            explanation: false,
            explanation_scroll: 0,
        });
    }
    pub(super) fn choose_recovery(&mut self, choice: RecoveryChoice) {
        self.editing.recovery_dirty = true;
        self.editing.visible = true;
        if let RecoveryChoice::Local(key) = choice {
            self.editing.local_loading = true;
            self.editing.local_request = Some(local_recovery::LocalRecoveryRequest::Load(key));
            return;
        }
        self.editing.queued = Some(match choice {
            RecoveryChoice::Historical {
                snapshot,
                target,
                revision,
                path,
                export,
            } => {
                if export {
                    InstructionManagementRequest::ExportRevision {
                        snapshot,
                        target,
                        revision,
                        path: Some(path),
                    }
                } else {
                    InstructionManagementRequest::Begin {
                        snapshot,
                        target,
                        action: InstructionEditAction::RestorePath { revision, path },
                    }
                }
            }

            RecoveryChoice::Local(_) => return,
            RecoveryChoice::Draft(scope, draft) => {
                InstructionManagementRequest::Resume { scope, draft }
            }
            RecoveryChoice::Operation(operation_id) => {
                InstructionManagementRequest::RepositoryReceipt { operation_id }
            }
        });
    }
}

impl InstructionManager {
    fn history_resource_action(&mut self, export: bool) {
        use super::menu::{Menu, MenuAction, MenuItem};
        let Some(snapshot) = self.snapshot_id() else {
            return;
        };
        let Some(target) = self.selected_target() else {
            return;
        };
        let revision = self
            .revision_selection
            .as_ref()
            .map(|selection| selection.from.clone())
            .or_else(|| {
                self.history
                    .get(self.history_selected)
                    .map(|entry| entry.commit.clone())
            });
        let Some(revision) = revision else {
            self.editing.status = "Choose a historical revision first.".into();
            return;
        };
        if matches!(target, InstructionInspectionTarget::Resource(_)) {
            self.editing.visible = true;
            self.editing.queued = Some(if export {
                InstructionManagementRequest::ExportRevision {
                    snapshot,
                    target,
                    revision,
                    path: None,
                }
            } else {
                InstructionManagementRequest::Begin {
                    snapshot,
                    target,
                    action: InstructionEditAction::Restore { revision },
                }
            });
        } else {
            let Some(entry) = self.history.iter().find(|entry| entry.commit == revision) else {
                self.editing.status = "Return to history and select this revision again.".into();
                return;
            };
            let items = entry.paths.iter().map(|path| MenuItem { label: path.clone(), hint: if export { "Export complete historical file or skill package into a new local directory. Source is unchanged." } else { "Restore this historical file or complete skill package as a reviewed new commit." }.into(), key: "Enter".into(), action: MenuAction::Recovery(RecoveryChoice::Historical { snapshot: snapshot.clone(), target: target.clone(), revision: revision.clone(), path: path.clone(), export }), disabled: None }).collect();
            self.editing.visible = false;
            self.menu = Some(Menu {
                title: "Choose historical path".into(),
                items,
                query: String::new(),
                selected: 0,
                context: Some(target),
                snapshot: Some(snapshot),
                revision: Some(revision),
                parent: None,
                explanation: false,
                explanation_scroll: 0,
            });
        }
    }
}
