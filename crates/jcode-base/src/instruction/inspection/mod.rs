//! Ephemeral read-only projection over the instruction, skill, repository and
//! roster owners. Nothing in this module installs instructions or publishes Git.
mod catalog;
mod detail;
mod overview;
mod presentation;
mod worker;
pub use worker::InspectionWorker;
pub use worker::InstructionTargetResolver;
pub(crate) use worker::ResolvedManagementTarget;

use super::*;
use crate::model_roster::RosterCatalog;
use crate::prompt::{PromptCapabilities, SkillInfo};
pub use jcode_instruction_types::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

type Result<T> = std::result::Result<T, InstructionInspectionFailure>;

/// Captured from the source-owning session, never supplied by a remote client.
#[derive(Clone)]
pub struct InspectionContext {
    pub session_id: String,
    pub working_dir: Option<PathBuf>,
    pub active_agent: Option<String>,
    pub stored_system: Option<Arc<str>>,
    pub is_selfdev: bool,
    pub capabilities: PromptCapabilities,
    pub roster_catalog: Option<Arc<RosterCatalog>>,
}

impl InspectionContext {
    pub fn from_session(
        session: &crate::session::Session,
        provider: &dyn crate::provider::Provider,
        is_selfdev: bool,
    ) -> Self {
        Self {
            session_id: session.id.clone(),
            working_dir: session.working_dir.as_deref().map(PathBuf::from),
            active_agent: session
                .active_agent()
                .map(|agent| format!("{}:{}", agent.scope, agent.id)),
            stored_system: session.system_prompt_text().map(Arc::from),
            is_selfdev,
            capabilities: PromptCapabilities::current(),
            roster_catalog: RosterCatalog::from_provider(provider).ok().map(Arc::new),
        }
    }
}

struct Repository {
    row: InstructionRepositoryRow,
    reference: Option<InstructionRepositoryRef>,
    state: Option<InstructionRepositoryState>,
    detail: String,
}

struct Resource {
    row: InstructionRow,
    path: PathBuf,
    managed: Option<InstructionResourceRef>,
    alias: Option<String>,
    annotation: String,
}

struct Document {
    id: String,
    title: String,
    text: String,
}

/// One connection owns one inspection. Exact detail is retained only until the
/// next selection, refresh, cancellation or close. History is pinned to the
/// inspected repository HEAD. No instruction-version state enters a Session.
pub struct InstructionInspector {
    repositories: InstructionRepositoryService,
    context: InspectionContext,
    snapshot: String,
    stores: BTreeMap<String, Repository>,
    resources: BTreeMap<String, Resource>,
    sources: InstructionSources,
    skills: Vec<SkillInfo>,
    consumers: Vec<ConsumerRegistration>,
    document: Option<Document>,
}

fn fail(operation: &str, detail: impl std::fmt::Display) -> InstructionInspectionFailure {
    InstructionInspectionFailure {
        operation: operation.into(),
        detail: detail.to_string(),
        refresh_required: false,
    }
}
fn canceled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(fail("inspect", "Inspection canceled"))
    } else {
        Ok(())
    }
}

impl InstructionInspector {
    pub fn open(
        repositories: InstructionRepositoryService,
        context: InspectionContext,
        cancel: &AtomicBool,
    ) -> Result<Self> {
        Self::collect(repositories, context, cancel)
    }

    pub fn snapshot(&self, filter: &InstructionFilter) -> InstructionInspectionSnapshot {
        InstructionInspectionSnapshot {
            snapshot: self.snapshot.clone(),
            session_id: self.context.session_id.clone(),
            active_agent: self.context.active_agent.clone(),
            repositories: self
                .stores
                .values()
                .map(|store| store.row.clone())
                .collect(),
            resources: self.rows(filter, 0),
        }
    }

    pub fn request(
        &mut self,
        request: InstructionInspectionRequest,
        cancel: &AtomicBool,
    ) -> InstructionInspectionReply {
        let result = self
            .handle(request, cancel)
            .unwrap_or_else(InstructionInspectionResult::Failed);
        InstructionInspectionReply {
            session_id: self.context.session_id.clone(),
            snapshot: Some(self.snapshot.clone()),
            result,
        }
    }

    fn check_snapshot(&self, snapshot: &str) -> Result<()> {
        if snapshot == self.snapshot {
            Ok(())
        } else {
            Err(InstructionInspectionFailure {
                operation: "inspect".into(),
                detail: "Inspection expired. Refresh to read authoritative state.".into(),
                refresh_required: true,
            })
        }
    }

    fn handle(
        &mut self,
        request: InstructionInspectionRequest,
        cancel: &AtomicBool,
    ) -> Result<InstructionInspectionResult> {
        canceled(cancel)?;
        match request {
            InstructionInspectionRequest::Resources {
                snapshot,
                filter,
                offset,
            } => {
                self.check_snapshot(&snapshot)?;
                Ok(InstructionInspectionResult::Resources(
                    self.rows(&filter, offset),
                ))
            }
            InstructionInspectionRequest::Detail {
                snapshot,
                target,
                view,
                revision,
            } => {
                self.check_snapshot(&snapshot)?;
                self.document = None;
                let (title, text) = self.detail(&target, view, revision.as_ref(), cancel)?;
                canceled(cancel)?;
                self.document = Some(Document {
                    id: uuid::Uuid::new_v4().to_string(),
                    title,
                    text,
                });
                Ok(InstructionInspectionResult::Text(self.text_page(None, 0)?))
            }
            InstructionInspectionRequest::Text {
                snapshot,
                document,
                offset,
            } => {
                self.check_snapshot(&snapshot)?;
                Ok(InstructionInspectionResult::Text(
                    self.text_page(Some(&document), offset)?,
                ))
            }
            InstructionInspectionRequest::History {
                snapshot,
                target,
                offset,
            } => {
                self.check_snapshot(&snapshot)?;
                Ok(InstructionInspectionResult::History(
                    self.history(&target, offset)?,
                ))
            }
            InstructionInspectionRequest::Cancel => {
                self.document = None;
                Ok(InstructionInspectionResult::Canceled)
            }
            InstructionInspectionRequest::Close => {
                self.document = None;
                Ok(InstructionInspectionResult::Closed)
            }
            InstructionInspectionRequest::Open { .. } => {
                Err(fail("open", "Open requires a fresh session context"))
            }
        }
    }

    fn text_page(&self, document: Option<&str>, offset: usize) -> Result<InstructionTextPage> {
        let value = self.document.as_ref().ok_or_else(|| {
            fail(
                "read detail",
                "No captured detail. Select a resource again.",
            )
        })?;
        if document.is_some_and(|id| id != value.id) {
            return Err(fail(
                "read detail",
                "Detail expired. Select the resource again.",
            ));
        }
        if offset > value.text.len() || !value.text.is_char_boundary(offset) {
            return Err(fail("read detail", "Invalid UTF-8 page offset"));
        }
        let mut end = offset.saturating_add(TEXT_PAGE_BYTES).min(value.text.len());
        while !value.text.is_char_boundary(end) {
            end -= 1;
        }
        Ok(InstructionTextPage {
            document: value.id.clone(),
            title: value.title.clone(),
            offset,
            total_bytes: value.text.len(),
            next: (end < value.text.len()).then_some(end),
            text: value.text[offset..end].into(),
        })
    }

    fn resource(&self, key: &str) -> Result<&Resource> {
        self.resources.get(key).ok_or_else(|| {
            fail(
                "select resource",
                "Resource does not belong to this inspection",
            )
        })
    }
    fn store(&self, key: &str) -> Result<&Repository> {
        self.stores.get(key).ok_or_else(|| {
            fail(
                "select repository",
                "Repository does not belong to this inspection",
            )
        })
    }
}

#[cfg(test)]
mod tests;
