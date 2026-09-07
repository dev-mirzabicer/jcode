use super::*;
use jcode_instruction_types::{
    InstructionConflictFile, InstructionDraftConflict, InstructionEditScope,
    InstructionRetainedDraft,
};
use serde::Deserialize;

#[derive(Clone)]
pub(super) struct ComparedDraft {
    token: String,
    generation: u64,
    current: Vec<InstructionDraft>,
}

impl InstructionDraftWorkspace {
    pub fn compare_current(
        &mut self,
        service: &InstructionRepositoryService,
        id: &str,
        generation: u64,
    ) -> InstructionRepositoryResult<InstructionDraftConflict> {
        let record = self.current(id, generation)?;
        let mut current = Vec::new();
        let mut files = Vec::new();
        for base in &record.bases {
            let capture = service.open_draft(&record.repository, &base.relative_path)?;
            let proposed = record
                .request
                .mutations
                .iter()
                .find_map(|mutation| match mutation {
                    InstructionFileMutation::Write {
                        relative_path,
                        content,
                    } if relative_path == &base.relative_path => {
                        Some(String::from_utf8(content.clone()).map(Some))
                    }
                    InstructionFileMutation::Delete { relative_path }
                        if relative_path == &base.relative_path =>
                    {
                        Some(Ok(None))
                    }
                    _ => None,
                })
                .ok_or_else(|| draft_error("Comparison requires complete write/delete mutations"))?
                .map_err(|error| draft_error(&format!("Draft content is not UTF-8: {error}")))?;
            files.push(InstructionConflictFile {
                path: base.relative_path.to_string_lossy().into_owned(),
                base: base.content.clone(),
                working: capture.content.clone(),
                proposed,
            });
            current.push(capture);
        }
        let first = current
            .first()
            .ok_or_else(|| draft_error("Draft contains no captured paths"))?;
        if current.iter().any(|value| {
            value.base_head != first.base_head || value.base_branch != first.base_branch
        }) {
            return Err(draft_error(
                "Repository changed during comparison; compare again",
            ));
        }
        for value in &current {
            service.validate_draft(value)?;
        }
        let token = uuid::Uuid::new_v4().to_string();
        let result = InstructionDraftConflict {
            draft: id.into(),
            generation,
            comparison: token.clone(),
            head: first.base_head.clone(),
            branch: first.base_branch.clone(),
            files,
        };
        self.comparison = Some(ComparedDraft {
            token,
            generation,
            current,
        });
        Ok(result)
    }

    /// Creates a new private draft based on the exact compared current state.
    /// The original draft and authoritative source are retained unchanged.
    pub fn reconcile_current(
        &mut self,
        service: &InstructionRepositoryService,
        id: &str,
        generation: u64,
        token: &str,
        use_working_content: bool,
    ) -> InstructionRepositoryResult<&InstructionEditingDraft> {
        let old = self.current(id, generation)?.clone();
        let compared = self
            .comparison
            .as_ref()
            .filter(|value| value.token == token && value.generation == generation)
            .ok_or_else(|| draft_error("Compare the draft with current source before reconciling"))?
            .clone();
        if old.outcome.is_some()
            || service
                .completed_operation_commit(&old.repository, &old.request.operation_id)?
                .is_some()
        {
            return Err(draft_error(
                "The original Save completed. Open a new edit from its current source instead.",
            ));
        }
        for capture in &compared.current {
            service.validate_draft(capture)?;
        }
        let paths = compared
            .current
            .iter()
            .map(|value| value.relative_path.clone())
            .collect::<Vec<_>>();
        let mut replacement = InstructionDraftWorkspace::default();
        let message = if use_working_content {
            "instruction: edit current source from recovered draft"
        } else {
            &old.request.message
        };
        let opened = replacement
            .begin(service, &old.repository, &old.session_id, &paths, message)?
            .clone();
        let same_base = opened
            .bases
            .iter()
            .zip(&compared.current)
            .all(|(left, right)| {
                left.base == right.base
                    && left.base_head == right.base_head
                    && left.base_branch == right.base_branch
            });
        if !same_base {
            replacement.discard(service, &opened.id, opened.generation)?;
            return Err(draft_error(
                "Source changed since comparison. Original draft is preserved; compare again.",
            ));
        }
        let mutations = if use_working_content {
            opened.request.mutations
        } else {
            old.request.mutations
        };
        replacement.revise(service, &opened.id, opened.generation, mutations)?;
        *self = replacement;
        self.draft()
            .ok_or_else(|| draft_error("Replacement draft is not attached"))
    }
}

#[derive(Deserialize)]
struct Header {
    schema: u32,
    id: String,
    session_id: String,
    generation: u64,
    request: RequestHeader,
    save_started: bool,
    outcome: Option<InstructionCommitOutcome>,
}
#[derive(Deserialize)]
struct RequestHeader {
    message: String,
}

impl InstructionRepositoryService {
    pub fn retained_drafts(
        &self,
        repository: &InstructionRepositoryRef,
        session: &str,
    ) -> InstructionRepositoryResult<(Vec<InstructionRetainedDraft>, Vec<String>)> {
        let directory = self
            .editing_draft_path(repository, &uuid::Uuid::nil().to_string())?
            .parent()
            .ok_or_else(|| draft_error("No draft directory"))?
            .to_path_buf();
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Vec::new(), Vec::new()));
            }
            Err(error) => {
                return Err(draft_error(&format!(
                    "Cannot list retained drafts: {error}"
                )));
            }
        };
        let mut drafts = Vec::new();
        let mut errors = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    errors.push(error.to_string());
                    continue;
                }
            };
            if entry.path().extension().is_none_or(|value| value != "json") {
                continue;
            }
            let read = || -> InstructionRepositoryResult<Header> {
                let file = read_record_file(&entry.path())?;
                serde_json::from_reader(std::io::BufReader::new(file))
                    .map_err(|error| draft_error(&format!("{}: {error}", entry.path().display())))
            };
            match read() {
                Ok(header) if header.session_id != session => {}
                Ok(header)
                    if header.schema == DRAFT_SCHEMA
                        && uuid::Uuid::parse_str(&header.id).is_ok()
                        && entry.path().file_stem().and_then(|value| value.to_str())
                            == Some(&header.id) =>
                {
                    drafts.push(InstructionRetainedDraft {
                        id: header.id,
                        scope: if repository.kind == InstructionRepositoryKind::Global {
                            InstructionEditScope::Global
                        } else {
                            InstructionEditScope::Project
                        },
                        subject: header.request.message,
                        generation: header.generation,
                        save_started: header.save_started,
                        committed: header.outcome.map(|value| value.commit),
                    });
                }
                Ok(_) => errors.push(format!(
                    "{} has unsupported or inconsistent draft metadata; it was retained",
                    entry.path().display()
                )),
                Err(error) => errors.push(error.to_string()),
            }
        }
        drafts.sort_by(|left, right| {
            left.subject
                .cmp(&right.subject)
                .then(left.id.cmp(&right.id))
        });
        Ok((drafts, errors))
    }
}
