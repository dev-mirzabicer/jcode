//! Durable unsaved intent. Kernel-owned draft leases block branch changes while
//! an editor is attached; dropping a connection releases the lease, not the draft.
use super::git::validate_operation_id;
use super::lease::{
    RepositoryMutationGuard, acquire_draft_lease, acquire_mutation_lease, active_drafts,
};
use super::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const DRAFT_SCHEMA: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstructionEditingDraft {
    schema: u32,
    pub id: String,
    pub session_id: String,
    pub generation: u64,
    pub repository: InstructionRepositoryRef,
    pub base_branch: Option<String>,
    pub bases: Vec<InstructionDraft>,
    pub request: InstructionCommitRequest,
    pub validated: bool,
    pub save_started: bool,
    pub outcome: Option<InstructionCommitOutcome>,
}

/// One attached client owns one editable draft. The record remains recoverable
/// after Drop, disconnect, close, or a failed Save. Only discard removes it.
#[derive(Default)]
pub struct InstructionDraftWorkspace {
    guard: Option<RepositoryMutationGuard>,
    attached: Option<InstructionEditingDraft>,
}

impl InstructionDraftWorkspace {
    pub fn draft(&self) -> Option<&InstructionEditingDraft> {
        self.attached.as_ref()
    }

    pub fn begin(
        &mut self,
        service: &InstructionRepositoryService,
        repository: &InstructionRepositoryRef,
        session_id: &str,
        paths: &[PathBuf],
        subject: &str,
    ) -> InstructionRepositoryResult<&InstructionEditingDraft> {
        if self.attached.is_some() {
            return Err(draft_error("Close or discard the current draft first"));
        }
        if paths.is_empty() {
            return Err(draft_error("A draft needs at least one target"));
        }
        let roots = service.roots()?;
        let id = uuid::Uuid::new_v4().to_string();
        let _mutation = acquire_mutation_lease(&roots.durable_state, repository, &id)?;
        let bases = paths
            .iter()
            .map(|path| service.open_draft(repository, path))
            .collect::<InstructionRepositoryResult<Vec<_>>>()?;
        let first = &bases[0];
        if bases
            .iter()
            .any(|base| base.base_head != first.base_head || base.base_branch != first.base_branch)
        {
            return Err(draft_error("Repository changed while opening the draft"));
        }
        let record = InstructionEditingDraft {
            schema: DRAFT_SCHEMA,
            id: id.clone(),
            session_id: session_id.into(),
            generation: 0,
            repository: repository.clone(),
            base_branch: first.base_branch.clone(),
            request: InstructionCommitRequest {
                operation_id: format!("draft-{id}-0"),
                message: subject.into(),
                expected_head: first.base_head.clone(),
                expected_files: bases.iter().map(|base| base.base.clone()).collect(),
                mutations: bases
                    .iter()
                    .map(|base| match &base.content {
                        Some(content) => InstructionFileMutation::Write {
                            relative_path: base.relative_path.clone(),
                            content: content.as_bytes().to_vec(),
                        },
                        None => InstructionFileMutation::Delete {
                            relative_path: base.relative_path.clone(),
                        },
                    })
                    .collect(),
            },
            bases,
            validated: false,
            save_started: false,
            outcome: None,
        };
        super::mutation::validate_request_paths(&record.request)?;
        let guard = acquire_draft_lease(&roots.durable_state, repository, &id)?;
        service.write_editing_draft(&record)?;
        self.guard = Some(guard);
        self.attached = Some(record);
        self.attached
            .as_ref()
            .ok_or_else(|| draft_error("Draft was not attached"))
    }

    pub fn resume(
        &mut self,
        service: &InstructionRepositoryService,
        repository: &InstructionRepositoryRef,
        session_id: &str,
        id: &str,
    ) -> InstructionRepositoryResult<&InstructionEditingDraft> {
        if self.attached.is_some() {
            return Err(draft_error("Close the current draft first"));
        }
        let roots = service.roots()?;
        let _mutation = acquire_mutation_lease(&roots.durable_state, repository, id)?;
        let record = service.read_editing_draft(repository, session_id, id)?;
        let guard = acquire_draft_lease(&roots.durable_state, repository, id)?;
        // Stale drafts must still open for comparison and recovery, not vanish.
        self.guard = Some(guard);
        self.attached = Some(record);
        self.attached
            .as_ref()
            .ok_or_else(|| draft_error("Draft was not attached"))
    }

    pub fn revise(
        &mut self,
        service: &InstructionRepositoryService,
        id: &str,
        generation: u64,
        mutations: Vec<InstructionFileMutation>,
    ) -> InstructionRepositoryResult<&InstructionEditingDraft> {
        let mut next = self.current(id, generation)?.clone();
        if next.outcome.is_some() || next.save_started {
            return Err(draft_error("This draft is already saved. Open a new edit."));
        }
        next.generation = generation
            .checked_add(1)
            .ok_or_else(|| draft_error("Draft generation exhausted"))?;
        next.request.operation_id = format!("draft-{}-{}", next.id, next.generation);
        next.request.mutations = mutations;
        super::mutation::validate_request_paths(&next.request)?;
        next.validated = false;
        service.write_editing_draft(&next)?;
        self.attached = Some(next);
        self.attached
            .as_ref()
            .ok_or_else(|| draft_error("Draft was not attached"))
    }

    pub fn review(
        &mut self,
        service: &InstructionRepositoryService,
        id: &str,
        generation: u64,
    ) -> InstructionRepositoryResult<InstructionCommitReview> {
        let mut next = self.current(id, generation)?.clone();
        for base in &next.bases {
            service.validate_draft(base)?;
        }
        let review = service.review_commit(&next.repository, &next.request)?;
        next.validated = review.errors.is_empty();
        service.write_editing_draft(&next)?;
        self.attached = Some(next);
        Ok(review)
    }

    pub fn save(
        &mut self,
        service: &InstructionRepositoryService,
        id: &str,
        generation: u64,
    ) -> InstructionRepositoryResult<InstructionCommitOutcome> {
        let mut next = self.current(id, generation)?.clone();
        if let Some(outcome) = &next.outcome {
            service.write_editing_draft(&next)?;
            self.guard = None;
            return Ok(outcome.clone());
        }
        let outcome = if let Some(commit) =
            service.completed_operation_commit(&next.repository, &next.request.operation_id)?
        {
            InstructionCommitOutcome {
                disposition: InstructionCommitDisposition::AlreadyCommitted,
                commit,
                changed_paths: super::mutation::affected_paths(&next.request.mutations),
            }
        } else {
            if !next.validated {
                return Err(draft_error(
                    "Review and validate this exact draft before Save",
                ));
            }
            let review = if next.save_started {
                service.review_interrupted_commit(&next.repository, &next.request)?
            } else {
                for base in &next.bases {
                    service.validate_draft(base)?;
                }
                service.review_commit(&next.repository, &next.request)?
            };
            if !review.errors.is_empty() {
                return Err(draft_error(&review.errors.join("\n")));
            }
            let branch = next.base_branch.as_deref().ok_or_else(|| draft_error("Save is unavailable on a detached repository. Close the draft and select a branch."))?;
            next.save_started = true;
            service.write_editing_draft(&next)?;
            self.attached = Some(next.clone());
            service.commit_on_branch(&next.repository, &next.request, Some(branch))?
        };
        next.outcome = Some(outcome.clone());
        // Preserve the receipt in memory even if its local recovery record fails.
        // A retry can also recover the structural operation from Git.
        self.attached = Some(next.clone());
        service.write_editing_draft(&next).map_err(|error| {
            InstructionRepositoryError::new(
                InstructionRepositoryErrorKind::Io,
                "record completed instruction Save",
                format!(
                    "Commit {} is published. Draft receipt needs repair: {error}",
                    outcome.commit
                ),
            )
            .repository(&next.repository)
            .may_have_working_changes()
        })?;
        self.guard = None;
        Ok(outcome)
    }

    pub fn close(&mut self) {
        self.attached = None;
        self.guard = None;
    }

    pub fn discard(
        &mut self,
        service: &InstructionRepositoryService,
        id: &str,
        generation: u64,
    ) -> InstructionRepositoryResult<()> {
        let current = self.current(id, generation)?;
        let path = service.editing_draft_path(&current.repository, id)?;
        std::fs::remove_file(&path)
            .map_err(|error| draft_error(&format!("Could not discard draft: {error}")))?;
        self.close();
        Ok(())
    }

    fn current(
        &self,
        id: &str,
        generation: u64,
    ) -> InstructionRepositoryResult<&InstructionEditingDraft> {
        self.attached
            .as_ref()
            .filter(|record| record.id == id && record.generation == generation)
            .ok_or_else(|| draft_error("Draft identity or generation changed; refresh the draft"))
    }
}

impl InstructionRepositoryService {
    fn editing_draft_path(
        &self,
        repository: &InstructionRepositoryRef,
        id: &str,
    ) -> InstructionRepositoryResult<PathBuf> {
        uuid::Uuid::parse_str(id).map_err(|_| draft_error("Invalid draft identity"))?;
        validate_operation_id(&repository.id)?;
        Ok(self
            .roots()?
            .durable_state
            .join("instruction-repositories")
            .join("drafts")
            .join(&repository.id)
            .join(format!("{id}.json")))
    }

    fn write_editing_draft(
        &self,
        record: &InstructionEditingDraft,
    ) -> InstructionRepositoryResult<()> {
        let path = self.editing_draft_path(&record.repository, &record.id)?;
        if let Some(parent) = path.parent() {
            crate::storage::ensure_dir(parent).map_err(|error| draft_error(&error.to_string()))?;
        }
        crate::storage::write_json_secret(&path, record)
            .map_err(|error| draft_error(&format!("Could not preserve draft: {error}")))
    }

    pub fn read_editing_draft(
        &self,
        repository: &InstructionRepositoryRef,
        session_id: &str,
        id: &str,
    ) -> InstructionRepositoryResult<InstructionEditingDraft> {
        let path = self.editing_draft_path(repository, id)?;
        let bytes = read_record_bytes(&path)?;
        let record: InstructionEditingDraft = serde_json::from_slice(&bytes).map_err(|error| {
            draft_error(&format!(
                "Draft recovery record is invalid and remains at {}: {error}",
                path.display()
            ))
        })?;
        if record.schema != DRAFT_SCHEMA
            || record.id != id
            || record.session_id != session_id
            || record.repository != *repository
        {
            return Err(draft_error(
                "Draft does not belong to this session and configured repository, or its schema is unsupported",
            ));
        }
        super::mutation::validate_request_paths(&record.request)?;
        if record.request.operation_id != format!("draft-{}-{}", record.id, record.generation)
            || record.request.expected_files
                != record
                    .bases
                    .iter()
                    .map(|base| base.base.clone())
                    .collect::<Vec<_>>()
        {
            return Err(draft_error(
                "Draft operation and captured file identities are inconsistent",
            ));
        }
        for base in &record.bases {
            if base.repository != *repository
                || base.base_head != record.request.expected_head
                || base.base_branch != record.base_branch
            {
                return Err(draft_error("Draft base identity is inconsistent"));
            }
        }
        Ok(record)
    }

    /// Called under the repository mutation lease before a branch transition.
    pub(super) fn require_no_attached_drafts(
        &self,
        repository: &InstructionRepositoryRef,
    ) -> InstructionRepositoryResult<()> {
        if !active_drafts(&self.roots()?.durable_state, repository)?.is_empty() {
            return Err(InstructionRepositoryError::new(InstructionRepositoryErrorKind::MutationBusy, "change instruction branch", "An attached draft is open in this Git repository. Close or discard it before changing branches.").repository(repository));
        }
        Ok(())
    }
}

fn read_record_bytes(path: &Path) -> InstructionRepositoryResult<Vec<u8>> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|error| draft_error(&error.to_string()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(draft_error("Draft record must be a regular file"));
    }
    std::fs::read(path).map_err(|error| draft_error(&error.to_string()))
}
fn draft_error(detail: &str) -> InstructionRepositoryError {
    InstructionRepositoryError::new(
        InstructionRepositoryErrorKind::Configuration,
        "manage instruction draft",
        detail,
    )
}
