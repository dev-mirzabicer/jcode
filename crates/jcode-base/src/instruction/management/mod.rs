//! Server/local manager orchestration over the instruction and Git owners.
//! Mutations are explicit, never canceled as though an in-flight commit rolled back.
mod metadata;
mod plans;
#[cfg(test)]
mod tests;
mod validation;
use super::inspection::{InspectionContext, InstructionTargetResolver, ResolvedManagementTarget};
use super::*;
pub use jcode_instruction_types::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

type Result<T> = std::result::Result<T, InstructionManagementFailure>;
fn fail(operation: &str, detail: impl std::fmt::Display) -> InstructionManagementFailure {
    InstructionManagementFailure {
        operation: operation.into(),
        detail: detail.to_string(),
        draft: None,
        source_unchanged: true,
    }
}
fn repo_error(error: InstructionRepositoryError) -> InstructionManagementFailure {
    InstructionManagementFailure {
        operation: error.operation.clone(),
        detail: error.to_string(),
        draft: None,
        source_unchanged: error.existing_state_unchanged,
    }
}
fn registrations() -> Result<Vec<ConsumerRegistration>> {
    let mut values =
        notification::registrations().map_err(|error| fail("consumer contracts", error))?;
    values.extend(workflow::registrations().map_err(|error| fail("consumer contracts", error))?);
    values.extend(composition_registrations().map_err(|error| fail("consumer contracts", error))?);
    Ok(values)
}
fn resolve_repository(
    service: &InstructionRepositoryService,
    context: &InspectionContext,
    scope: InstructionEditScope,
) -> Result<InstructionRepositoryRef> {
    match scope {
        InstructionEditScope::Global => service.global_repository().map_err(repo_error),
        InstructionEditScope::Project => context.working_dir.as_deref().ok_or_else(|| fail("project repository", "This session has no project directory"))
            .and_then(|path| service.resolve_project_repository(path).map_err(repo_error))?
            .ok_or_else(|| fail("project repository", "Set up this project's instruction repository before creating project instructions")),
    }
}

#[derive(Default)]
struct ManagerState {
    workspace: InstructionDraftWorkspace,
    context: Option<InspectionContext>,
    warnings: Vec<String>,
    choices: InstructionEditChoices,
}

#[derive(Default, Clone)]
pub struct InstructionManagementWorker {
    state: Arc<Mutex<ManagerState>>,
}
impl InstructionManagementWorker {
    pub fn submit(
        &self,
        repositories: InstructionRepositoryService,
        context: InspectionContext,
        resolver: InstructionTargetResolver,
        request: InstructionManagementRequest,
    ) -> tokio::sync::oneshot::Receiver<InstructionManagementReply> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let state = Arc::clone(&self.state);
        tokio::task::spawn_blocking(move || {
            let session_id = context.session_id.clone();
            let result = match state.try_lock() {
                Ok(mut state) => {
                    let result = state.handle(&repositories, context, &resolver, request);
                    result.unwrap_or_else(|mut error| {
                        error.draft = state.workspace.draft().map(|record| record.id.clone());
                        InstructionManagementResult::Failed(error)
                    })
                }
                Err(_) => InstructionManagementResult::Failed(fail(
                    "manage instructions",
                    "Another manager operation is still running. Its result is not canceled; wait or recover its receipt.",
                )),
            };
            let _ = sender.send(InstructionManagementReply { session_id, result });
        });
        receiver
    }
}

impl ManagerState {
    fn handle(
        &mut self,
        service: &InstructionRepositoryService,
        context: InspectionContext,
        resolver: &InstructionTargetResolver,
        request: InstructionManagementRequest,
    ) -> Result<InstructionManagementResult> {
        if self
            .workspace
            .draft()
            .is_some_and(|record| record.session_id != context.session_id)
        {
            return Err(fail(
                "manager session",
                "The attached draft belongs to another session. Close it before switching.",
            ));
        }
        match request {
            InstructionManagementRequest::RepositoryChoices { scope } => {
                Ok(InstructionManagementResult::RepositoryChoices(
                    service
                        .repository_operation_choices(context.working_dir.as_deref(), scope)
                        .map_err(repo_error)?,
                ))
            }
            InstructionManagementRequest::PlanRepository { scope, action } => {
                if self.workspace.draft().is_some() {
                    return Err(fail(
                        "repository operation",
                        "Close and preserve the current draft before changing repository configuration.",
                    ));
                }
                Ok(InstructionManagementResult::RepositoryPlan(
                    service
                        .plan_repository_operation(
                            &context.session_id,
                            context.working_dir.as_deref(),
                            scope,
                            action,
                        )
                        .map_err(repo_error)?,
                ))
            }
            InstructionManagementRequest::ApplyRepository { operation_id } => {
                if self.workspace.draft().is_some() {
                    return Err(fail(
                        "repository operation",
                        "Close and preserve the current draft before applying a repository operation.",
                    ));
                }
                Ok(InstructionManagementResult::RepositoryReceipt(
                    service
                        .apply_repository_operation(
                            &context.session_id,
                            context.working_dir.as_deref(),
                            &operation_id,
                        )
                        .map_err(repo_error)?,
                ))
            }
            InstructionManagementRequest::RepositoryReceipt { operation_id } => {
                Ok(InstructionManagementResult::RepositoryReceipt(
                    service
                        .repository_operation_receipt(
                            &context.session_id,
                            context.working_dir.as_deref(),
                            &operation_id,
                        )
                        .map_err(repo_error)?,
                ))
            }
            InstructionManagementRequest::Begin {
                snapshot,
                target,
                action,
            } => {
                let resolved = resolver
                    .resolve(&context.session_id, &snapshot, &target)
                    .map_err(|error| fail("select edit target", error.detail))?;
                if resolved.context.working_dir != context.working_dir {
                    return Err(fail(
                        "select edit target",
                        "Project context changed. Refresh inspection.",
                    ));
                }
                let plan = plans::plan(service, &resolved, action)?;
                let paths = plan.files.keys().cloned().collect::<Vec<_>>();
                let opened = self
                    .workspace
                    .begin(
                        service,
                        &plan.repository,
                        &context.session_id,
                        &paths,
                        &plan.subject,
                    )
                    .map_err(repo_error)?
                    .clone();
                for base in &opened.bases {
                    if plan.observed.get(&base.relative_path) != Some(&base.content) {
                        self.workspace.close();
                        return Err(fail(
                            "open edit",
                            "Source changed while preparing the draft. Refresh before editing.",
                        ));
                    }
                }
                let mutations = plan
                    .files
                    .into_iter()
                    .map(|(relative_path, content)| match content {
                        Some(content) => InstructionFileMutation::Write {
                            relative_path,
                            content: content.into_bytes(),
                        },
                        None => InstructionFileMutation::Delete { relative_path },
                    })
                    .collect();
                self.workspace
                    .revise(service, &opened.id, opened.generation, mutations)
                    .map_err(repo_error)?;
                self.context = Some(context);
                self.warnings = plan.warnings;
                self.capture_choices(service)?;
                Ok(InstructionManagementResult::Draft(self.snapshot()?))
            }
            InstructionManagementRequest::Update {
                draft,
                generation,
                change,
            } => {
                self.check(&draft, generation)?;
                let current = self
                    .workspace
                    .draft()
                    .ok_or_else(|| fail("update draft", "No attached draft"))?;
                if current.save_started {
                    return Err(fail(
                        "update draft",
                        "Resolve the previous Save outcome before changing this draft",
                    ));
                }
                let file = match &change {
                    InstructionDraftChange::Body { file, .. }
                    | InstructionDraftChange::Metadata { file, .. }
                    | InstructionDraftChange::RepairSource { file, .. } => file,
                };
                let mut mutations = current.request.mutations.clone();
                let mut found = false;
                for mutation in &mut mutations {
                    if let InstructionFileMutation::Write {
                        relative_path,
                        content,
                    } = mutation
                        && relative_path.to_str() == Some(file)
                    {
                        let source = std::str::from_utf8(content)
                            .map_err(|error| fail("read draft", error))?;
                        *content =
                            metadata::apply(&current.repository, relative_path, source, &change)?
                                .into_bytes();
                        found = true;
                    }
                }
                if !found {
                    return Err(fail(
                        "update draft",
                        "Selected file is absent or scheduled for deletion",
                    ));
                }
                self.workspace
                    .revise(service, &draft, generation, mutations)
                    .map_err(repo_error)?;
                Ok(InstructionManagementResult::Draft(self.snapshot()?))
            }
            InstructionManagementRequest::Review { draft, generation } => {
                self.check(&draft, generation)?;
                self.check_repository(service, &context)?;
                let mut review = self
                    .workspace
                    .review(service, &draft, generation)
                    .map_err(repo_error)?;
                let previews = self.validate(service, &mut review)?;
                let mut snapshot = self.snapshot()?;
                snapshot.reviewed = review.errors.is_empty();
                let files = review
                    .files
                    .into_iter()
                    .map(|file| {
                        let utf8 = |bytes: Option<Vec<u8>>| {
                            bytes
                                .map(String::from_utf8)
                                .transpose()
                                .map_err(|error| fail("review content", error))
                        };
                        Ok(InstructionEditComparison {
                            path: file.relative_path.to_string_lossy().into_owned(),
                            working: utf8(file.working)?,
                            committed: utf8(file.committed)?,
                            proposed: utf8(file.proposed)?,
                        })
                    })
                    .collect::<Result<_>>()?;
                Ok(InstructionManagementResult::Reviewed(
                    InstructionEditReview {
                        draft: snapshot,
                        files,
                        errors: review.errors,
                        previews,
                    },
                ))
            }
            InstructionManagementRequest::Save { draft, generation } => {
                self.check(&draft, generation)?;
                self.check_repository(service, &context)?;
                let current = self
                    .workspace
                    .draft()
                    .ok_or_else(|| fail("save draft", "No draft"))?;
                if !current.save_started && current.outcome.is_none() {
                    if !current.validated {
                        return Err(fail("save draft", "Review this exact draft before Save"));
                    }
                    let mut review = service
                        .review_commit(&current.repository, &current.request)
                        .map_err(repo_error)?;
                    self.validate(service, &mut review)?;
                    if !review.errors.is_empty() {
                        return Err(fail("validate Save", review.errors.join("\n")));
                    }
                }
                let outcome = self
                    .workspace
                    .save(service, &draft, generation)
                    .map_err(repo_error)?;
                Ok(InstructionManagementResult::Saved {
                    draft,
                    commit: outcome.commit,
                    no_change: outcome.disposition == InstructionCommitDisposition::NoChange,
                    recovered: outcome.disposition
                        == InstructionCommitDisposition::AlreadyCommitted,
                    paths: outcome
                        .changed_paths
                        .iter()
                        .map(|path| path.to_string_lossy().into_owned())
                        .collect(),
                })
            }
            InstructionManagementRequest::Resume { scope, draft } => {
                let repository = resolve_repository(service, &context, scope)?;
                self.workspace
                    .resume(service, &repository, &context.session_id, &draft)
                    .map_err(repo_error)?;
                self.context = Some(context);
                self.warnings = vec!["Recovered unsaved intent. Review current source and any previous Save receipt before continuing.".into()];
                self.capture_choices(service)?;
                Ok(InstructionManagementResult::Draft(self.snapshot()?))
            }
            InstructionManagementRequest::Close => {
                self.workspace.close();
                self.context = None;
                self.warnings.clear();
                Ok(InstructionManagementResult::Closed)
            }
            InstructionManagementRequest::Discard { draft, generation } => {
                self.workspace
                    .discard(service, &draft, generation)
                    .map_err(repo_error)?;
                self.context = None;
                self.warnings.clear();
                Ok(InstructionManagementResult::Discarded)
            }
        }
    }
    fn check(&self, id: &str, generation: u64) -> Result<()> {
        if self
            .workspace
            .draft()
            .is_some_and(|record| record.id == id && record.generation == generation)
        {
            Ok(())
        } else {
            Err(fail(
                "draft identity",
                "Draft or generation changed. Recover the current draft.",
            ))
        }
    }
    fn check_repository(
        &self,
        service: &InstructionRepositoryService,
        context: &InspectionContext,
    ) -> Result<()> {
        let current = self
            .workspace
            .draft()
            .ok_or_else(|| fail("repository", "No attached draft"))?;
        let scope = if current.repository.kind == InstructionRepositoryKind::Global {
            InstructionEditScope::Global
        } else {
            InstructionEditScope::Project
        };
        let resolved = resolve_repository(service, context, scope)?;
        if resolved != current.repository {
            return Err(fail(
                "repository",
                "Configured repository changed since draft creation. Source remains untouched.",
            ));
        }
        Ok(())
    }
    fn snapshot(&self) -> Result<InstructionEditDraft> {
        let record = self
            .workspace
            .draft()
            .ok_or_else(|| fail("draft", "No attached draft"))?;
        let files = record
            .request
            .mutations
            .iter()
            .map(|mutation| match mutation {
                InstructionFileMutation::Write {
                    relative_path,
                    content,
                } => {
                    let source = std::str::from_utf8(content)
                        .map_err(|error| fail("draft content", error))?;
                    Ok(metadata::file(
                        &record.repository,
                        relative_path,
                        Some(source),
                    ))
                }
                InstructionFileMutation::Delete { relative_path } => {
                    let original = record
                        .bases
                        .iter()
                        .find(|base| &base.relative_path == relative_path)
                        .and_then(|base| base.content.as_deref());
                    let mut file = metadata::file(&record.repository, relative_path, original);
                    file.deleted = true;
                    Ok(file)
                }
                InstructionFileMutation::Rename { .. } => Err(fail(
                    "draft representation",
                    "Rename must be represented by reviewed complete file versions",
                )),
            })
            .collect::<Result<_>>()?;
        Ok(InstructionEditDraft {
            id: record.id.clone(),
            generation: record.generation,
            title: record.request.message.clone(),
            scope: if record.repository.kind == InstructionRepositoryKind::Global {
                InstructionEditScope::Global
            } else {
                InstructionEditScope::Project
            },
            repository: record.repository.root.to_string_lossy().into_owned(),
            branch: record.base_branch.clone(),
            files,
            subject: record.request.message.clone(),
            warnings: self.warnings.clone(),
            reviewed: record.validated,
            save_started: record.save_started,
            choices: self.choices.clone(),
        })
    }
    fn capture_choices(&mut self, service: &InstructionRepositoryService) -> Result<()> {
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| fail("edit choices", "No session context"))?;
        let project = context
            .working_dir
            .as_deref()
            .map(|path| service.resolve_project_repository(path))
            .transpose()
            .map_err(repo_error)?
            .flatten();
        let runtime = InstructionRuntime::discover(
            service
                .instruction_sources(project.as_ref())
                .map_err(repo_error)?,
        );
        self.choices = InstructionEditChoices {
            models: context
                .roster_catalog
                .as_ref()
                .map(|catalog| catalog.qualified_models())
                .unwrap_or_default(),
            ..Default::default()
        };
        for summary in runtime.resources() {
            let reference = metadata::selector_text(&metadata::selector(&summary.resource));
            match summary.resource.kind {
                InstructionKind::Agent => self.choices.agents.push(reference),
                InstructionKind::Module => self.choices.modules.push(reference),
                _ => {}
            }
        }
        Ok(())
    }
    fn validate(
        &self,
        service: &InstructionRepositoryService,
        review: &mut InstructionCommitReview,
    ) -> Result<Vec<InstructionEditPreview>> {
        let record = self
            .workspace
            .draft()
            .ok_or_else(|| fail("validate draft", "No attached draft"))?;
        let (_, (errors, previews)) = service
            .review_commit_with(&record.repository, &record.request, |sources| {
                validation::previews(sources, &record.request).map_err(|error| {
                    InstructionRepositoryError::new(
                        InstructionRepositoryErrorKind::Configuration,
                        "validate draft previews",
                        error.detail,
                    )
                })
            })
            .map_err(repo_error)?;
        review.errors.extend(errors);
        Ok(previews)
    }
}
