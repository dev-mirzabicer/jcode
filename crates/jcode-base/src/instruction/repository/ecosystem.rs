//! Dedicated AGENTS.md working-file edits. Never adopts or commits a parent Git repository.
use super::mutation::{atomic_write_path, sha256};
use super::review::read_bytes;
use super::*;
use jcode_instruction_types::*;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    id: String,
    session: String,
    scope: InstructionEditScope,
    path: PathBuf,
    generation: u64,
    base: Option<String>,
    proposed: String,
    reviewed: bool,
    started: bool,
    saved: bool,
}
pub struct EcosystemWorkspace {
    record: Record,
    _lease: File,
}
impl InstructionRepositoryService {
    fn ecosystem_path(
        &self,
        project: Option<&Path>,
        scope: InstructionEditScope,
    ) -> InstructionRepositoryResult<PathBuf> {
        match scope {
            InstructionEditScope::Global => self.global_agents_path(),
            InstructionEditScope::Project => self
                .resolve_project_root(project.ok_or_else(|| error("Session has no project"))?)
                .map(|root| root.join("AGENTS.md")),
        }
    }
    fn ecosystem_record_path(
        &self,
        session: &str,
        id: &str,
    ) -> InstructionRepositoryResult<PathBuf> {
        uuid::Uuid::parse_str(
            id.strip_prefix("ecosystem-")
                .ok_or_else(|| error("Invalid ecosystem draft ID"))?,
        )
        .map_err(|_| error("Invalid ecosystem draft ID"))?;
        Ok(self
            .roots()?
            .durable_state
            .join("instruction-repositories/ecosystem-drafts")
            .join(sha256(session.as_bytes()))
            .join(format!("{id}.json")))
    }
    fn persist_ecosystem(&self, record: &Record) -> InstructionRepositoryResult<()> {
        let path = self.ecosystem_record_path(&record.session, &record.id)?;
        crate::storage::ensure_dir(path.parent().ok_or_else(|| error("Missing draft parent"))?)
            .map_err(|e| error(&e.to_string()))?;
        crate::storage::write_json_secret(&path, record).map_err(|e| error(&e.to_string()))
    }
    pub fn open_ecosystem(
        &self,
        session: &str,
        project: Option<&Path>,
        scope: InstructionEditScope,
        selected: &Path,
    ) -> InstructionRepositoryResult<EcosystemWorkspace> {
        let path = self.ecosystem_path(project, scope)?;
        let same_file = selected
            .canonicalize()
            .ok()
            .zip(path.canonicalize().ok())
            .is_some_and(|(selected, expected)| selected == expected);
        if selected.is_symlink() || (path != selected && !same_file) {
            return Err(error(
                "Selected file is not this session's dedicated AGENTS.md input",
            ));
        }
        let base = read_source(&path)?;
        let record = Record {
            schema: 1,
            id: format!("ecosystem-{}", uuid::Uuid::new_v4()),
            session: session.into(),
            scope,
            path,
            generation: 0,
            proposed: base.clone().unwrap_or_default(),
            base,
            reviewed: false,
            started: false,
            saved: false,
        };
        self.persist_ecosystem(&record)?;
        let lease = lock_file(
            &self
                .ecosystem_record_path(session, &record.id)?
                .with_extension("lock"),
        )?;
        Ok(EcosystemWorkspace {
            record,
            _lease: lease,
        })
    }
    pub fn resume_ecosystem(
        &self,
        session: &str,
        project: Option<&Path>,
        id: &str,
    ) -> InstructionRepositoryResult<EcosystemWorkspace> {
        let path = self.ecosystem_record_path(session, id)?;
        let lease = lock_file(&path.with_extension("lock"))?;
        let file = open_regular(&path)?;
        let record: Record = serde_json::from_reader(std::io::BufReader::new(file))
            .map_err(|e| error(&format!("Invalid retained ecosystem draft: {e}")))?;
        if record.schema != 1
            || record.id != id
            || record.session != session
            || record.path != self.ecosystem_path(project, record.scope)?
        {
            return Err(error(
                "Ecosystem draft belongs to another session/project or unsupported schema",
            ));
        }
        Ok(EcosystemWorkspace {
            record,
            _lease: lease,
        })
    }
    pub fn retained_ecosystem(
        &self,
        session: &str,
    ) -> InstructionRepositoryResult<(Vec<InstructionRetainedDraft>, Vec<String>)> {
        let directory = self
            .ecosystem_record_path(session, &format!("ecosystem-{}", uuid::Uuid::nil()))?
            .parent()
            .ok_or_else(|| error("No recovery directory"))?
            .to_path_buf();
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Vec::new(), Vec::new()));
            }
            Err(e) => return Err(error(&e.to_string())),
        };
        #[derive(Deserialize)]
        struct Header {
            id: String,
            session: String,
            scope: InstructionEditScope,
            generation: u64,
            started: bool,
            saved: bool,
        }
        let mut rows = Vec::new();
        let mut errors = Vec::new();
        for entry in entries {
            let path = entry.map_err(|e| error(&e.to_string()))?.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let header = (|| -> InstructionRepositoryResult<Header> {
                let header: Header =
                    serde_json::from_reader(std::io::BufReader::new(open_regular(&path)?))
                        .map_err(|e| error(&e.to_string()))?;
                if header.session != session {
                    return Err(error("Invalid ecosystem recovery session"));
                }
                Ok(header)
            })();
            let header = match header {
                Ok(header) => header,
                Err(error) => {
                    errors.push(format!("{}: {error}; record retained", path.display()));
                    continue;
                }
            };
            rows.push(InstructionRetainedDraft {
                id: header.id,
                scope: header.scope,
                subject: "AGENTS.md working-file draft (no parent commit)".into(),
                generation: header.generation,
                save_started: header.started,
                committed: header
                    .saved
                    .then(|| "working file saved, not committed".into()),
            });
        }
        Ok((rows, errors))
    }
}
impl EcosystemWorkspace {
    pub fn snapshot(&self) -> InstructionEditDraft {
        let r = &self.record;
        InstructionEditDraft { working_file_only:true, id:r.id.clone(), generation:r.generation, title:"AGENTS.md working-file edit".into(), scope:r.scope, repository:r.path.display().to_string(), branch:None, files:vec![InstructionEditFile { executable:false, key:"AGENTS.md".into(), path:r.path.display().to_string(), body:r.proposed.clone(), metadata:InstructionEditMetadata::Ecosystem, deleted:false }], subject:"Save AGENTS.md without committing its parent repository".into(), warnings:vec!["This is a dedicated ecosystem input, not an instruction Git store. Save changes only its working file. No parent file is staged or committed. Existing session instructions remain frozen.".into()], reviewed:r.reviewed, save_started:r.started, committed:r.saved.then(|| "working file saved, not committed".into()), choices:InstructionEditChoices::default() }
    }
    pub fn handle(
        &mut self,
        service: &InstructionRepositoryService,
        session: &str,
        project: Option<&Path>,
        request: InstructionManagementRequest,
    ) -> InstructionRepositoryResult<InstructionManagementResult> {
        let before = self.record.clone();
        let result = self.handle_inner(service, session, project, request);
        if result
            .as_ref()
            .is_err_and(|error| error.existing_state_unchanged)
        {
            self.record = before;
        }
        result
    }
    fn handle_inner(
        &mut self,
        service: &InstructionRepositoryService,
        session: &str,
        project: Option<&Path>,
        request: InstructionManagementRequest,
    ) -> InstructionRepositoryResult<InstructionManagementResult> {
        let r = &mut self.record;
        if r.session != session || r.path != service.ecosystem_path(project, r.scope)? {
            return Err(error("Session/project changed; ecosystem draft retained"));
        }
        match request {
            InstructionManagementRequest::Update {
                draft,
                generation,
                change,
            } => {
                check(r, &draft, generation)?;
                if r.started {
                    return Err(error(
                        "Resolve the previous working-file Save before editing again",
                    ));
                }
                match change {
                    InstructionDraftChange::Body { file, body } if file == "AGENTS.md" => {
                        r.proposed = body
                    }
                    _ => {
                        return Err(error(
                            "AGENTS.md supports complete body editing, not managed metadata",
                        ));
                    }
                }
                r.generation = r
                    .generation
                    .checked_add(1)
                    .ok_or_else(|| error("Draft generation exhausted"))?;
                r.reviewed = false;
                service.persist_ecosystem(r)?;
                Ok(InstructionManagementResult::Draft(self.snapshot()))
            }
            InstructionManagementRequest::Review { draft, generation } => {
                check(r, &draft, generation)?;
                let current = read_source(&r.path)?;
                let errors = if current != r.base {
                    vec!["AGENTS.md changed outside this draft. The current and proposed versions are shown. Close this preserved draft and edit current source instead of overwriting it.".into()]
                } else {
                    Vec::new()
                };
                r.reviewed = errors.is_empty();
                service.persist_ecosystem(r)?;
                let file = InstructionEditComparison {
                    path: r.path.display().to_string(),
                    working: current,
                    committed: r.base.clone(),
                    proposed: Some(r.proposed.clone()),
                    working_executable: false,
                    committed_executable: false,
                    proposed_executable: false,
                };
                Ok(InstructionManagementResult::Reviewed(
                    InstructionEditReview {
                        draft: self.snapshot(),
                        files: vec![file],
                        errors,
                        previews: Vec::new(),
                    },
                ))
            }
            InstructionManagementRequest::Save { draft, generation } => {
                check(r, &draft, generation)?;
                if !r.reviewed {
                    return Err(error("Review this exact working-file draft before Save"));
                }
                let parent = r
                    .path
                    .parent()
                    .ok_or_else(|| error("AGENTS.md has no parent"))?;
                let canonical_parent = parent.canonicalize().map_err(|e| error(&e.to_string()))?;
                let directory = canonical_parent.join(".jcode/instruction-file-locks");
                safe_directory(&directory)?;
                let _file_lease = lock_file(&directory.join(format!(
                        "{}.lock",
                        sha256(
                            canonical_parent
                                .join("AGENTS.md")
                                .to_string_lossy()
                                .as_bytes()
                        )
                    )))?;
                if r.saved {
                    return Ok(InstructionManagementResult::FileSaved {
                        draft: r.id.clone(),
                        path: r.path.display().to_string(),
                        no_change: r.base.as_deref() == Some(r.proposed.as_str()),
                    });
                }
                let current = read_source(&r.path)?;
                if r.started && current.as_deref() == Some(r.proposed.as_str()) {
                    r.saved = true;
                    service.persist_ecosystem(r)?;
                    return Ok(InstructionManagementResult::FileSaved {
                        draft: r.id.clone(),
                        path: r.path.display().to_string(),
                        no_change: r.base.as_deref() == Some(r.proposed.as_str()),
                    });
                }
                if current != r.base {
                    return Err(error(
                        "AGENTS.md changed since review. Neither external work nor this draft was overwritten.",
                    ));
                }
                if current.as_deref() == Some(r.proposed.as_str()) {
                    r.started = true;
                    r.saved = true;
                    service.persist_ecosystem(r)?;
                    return Ok(InstructionManagementResult::FileSaved {
                        draft: r.id.clone(),
                        path: r.path.display().to_string(),
                        no_change: true,
                    });
                }
                r.started = true;
                service.persist_ecosystem(r)?;
                atomic_write_path(
                    &r.path,
                    r.proposed.as_bytes(),
                    r.scope == InstructionEditScope::Global,
                )
                .map_err(|e| error(&e.to_string()).may_have_working_changes())?;
                r.saved = true;
                service
                    .persist_ecosystem(r)
                    .map_err(InstructionRepositoryError::may_have_working_changes)?;
                Ok(InstructionManagementResult::FileSaved {
                    draft: r.id.clone(),
                    path: r.path.display().to_string(),
                    no_change: r.base.as_deref() == Some(r.proposed.as_str()),
                })
            }
            InstructionManagementRequest::Discard { draft, generation } => {
                check(r, &draft, generation)?;
                std::fs::remove_file(service.ecosystem_record_path(session, &draft)?)
                    .map_err(|e| error(&e.to_string()))?;
                Ok(InstructionManagementResult::Discarded)
            }
            InstructionManagementRequest::Close => Ok(InstructionManagementResult::Closed),
            _ => Err(error(
                "Close the ecosystem draft before starting another operation",
            )),
        }
    }
}
fn error(detail: &str) -> InstructionRepositoryError {
    InstructionRepositoryError::new(
        InstructionRepositoryErrorKind::Configuration,
        "edit ecosystem input",
        detail,
    )
}
fn check(r: &Record, id: &str, generation: u64) -> InstructionRepositoryResult<()> {
    if r.id != id || r.generation != generation {
        Err(error("Draft identity or generation changed"))
    } else {
        Ok(())
    }
}
fn reference(path: &Path) -> InstructionRepositoryResult<InstructionRepositoryRef> {
    Ok(InstructionRepositoryRef {
        id: "ecosystem-read".into(),
        kind: InstructionRepositoryKind::ProjectExternal,
        root: path
            .parent()
            .ok_or_else(|| error("No source parent"))?
            .to_path_buf(),
        project_root: None,
        project_config_path: None,
        configured_branch: None,
        configured_remote: None,
        owner_only: false,
    })
}
fn read_source(path: &Path) -> InstructionRepositoryResult<Option<String>> {
    let reference = reference(path)?;
    read_bytes(&reference, Path::new("AGENTS.md"))?
        .map(String::from_utf8)
        .transpose()
        .map_err(|e| {
            error(&format!(
                "AGENTS.md is not valid UTF-8; original bytes retained: {e}"
            ))
        })
}
fn open_regular(path: &Path) -> InstructionRepositoryResult<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|e| error(&e.to_string()))?;
    if !file
        .metadata()
        .map_err(|e| error(&e.to_string()))?
        .is_file()
    {
        return Err(error("Expected a regular file"));
    }
    Ok(file)
}
fn safe_directory(path: &Path) -> InstructionRepositoryResult<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(error("Editor lock directory crosses a symlink"));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&current).map_err(|e| error(&e.to_string()))?
            }
            Err(e) => return Err(error(&e.to_string())),
        }
    }
    crate::platform::set_directory_permissions_owner_only(path).map_err(|e| error(&e.to_string()))
}
fn lock_file(path: &Path) -> InstructionRepositoryResult<File> {
    if let Some(parent) = path.parent() {
        crate::storage::ensure_dir(parent).map_err(|e| error(&e.to_string()))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .mode(0o600);
    }
    let file = options.open(path).map_err(|e| error(&e.to_string()))?;
    if !file
        .metadata()
        .map_err(|e| error(&e.to_string()))?
        .is_file()
    {
        return Err(error("Editor lock is not a regular file"));
    }
    file.try_lock()
        .map_err(|e| error(&format!("Another client owns this edit: {e}")))?;
    Ok(file)
}
