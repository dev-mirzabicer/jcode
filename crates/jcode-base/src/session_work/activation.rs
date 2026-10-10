//! Activating session work for a new session. Activation happens once, while
//! the session is created and before its Session Context is written; the
//! fact is fixed for the session's lifetime.
use super::store::InitialRevision;
use super::{SessionWorkError, SessionWorkSurface, current_module_types};
use crate::instruction::notification::Notification;
use crate::instruction::{InstructionRepositoryService, SystemPromptComposer};
use crate::session::Session;
use jcode_session_work_types::{
    ActivationOrigin, RevisionSource, SessionWorkActivation, SessionWorkBinding, SessionWorkRole,
};
use std::path::Path;

/// The managed global skill every activated session is told to read.
pub const SESSION_WORK_SKILL: &str = "session-work";

/// How a new session relates to session work.
pub enum NewSessionWork<'a> {
    /// A new hosted primary session.
    Primary,
    /// A split of `source`'s conversation. It continues `source`'s activation
    /// without its workflow.
    Split { source: &'a Session },
    /// A transfer from `source`, which copies its workflow with provenance.
    Transfer { source: &'a Session },
    /// An isolated child created with a task preset and its optional template.
    Child {
        preset: &'a str,
        template: Option<&'a str>,
    },
}

/// Activate session work for `session` when the creation rules call for it.
/// Returns whether it was activated. Call before the Session Context message
/// is written; a creation that later fails removes the state through
/// `remove_unpublished_session`.
pub fn activate_new_session(
    session: &mut Session,
    kind: NewSessionWork<'_>,
    repositories: &InstructionRepositoryService,
) -> anyhow::Result<bool> {
    let surface = SessionWorkSurface::new()?;
    activate_with(session, kind, repositories, &surface)
}

pub(crate) fn activate_with(
    session: &mut Session,
    kind: NewSessionWork<'_>,
    repositories: &InstructionRepositoryService,
    surface: &SessionWorkSurface,
) -> anyhow::Result<bool> {
    let store = surface.store();
    let working_dir = session.working_dir.clone();
    let working_dir = working_dir.as_deref().map(Path::new);
    let now = chrono::Utc::now();
    let (role, origin, module_types, initial) = match kind {
        NewSessionWork::Split { source } => {
            let Some(binding) = &source.session_work else {
                return Ok(false);
            };
            // A split continues its source's conversation, including the
            // module types frozen into it.
            let module_types = super::frozen_module_types(store, &source.id)?;
            (
                binding.role,
                ActivationOrigin::Split {
                    source_session: source.id.clone(),
                },
                module_types,
                None,
            )
        }
        _ if !super::session_work_enabled() => return Ok(false),
        NewSessionWork::Primary => (
            SessionWorkRole::Primary,
            ActivationOrigin::Fresh {},
            current_module_types(repositories, working_dir)?,
            None,
        ),
        NewSessionWork::Transfer { source } => {
            let initial = match &source.session_work {
                Some(_) => store
                    .workflow_head(&source.id)?
                    .map(|head| InitialRevision {
                        text: head.revision.text,
                        source: RevisionSource::Transfer {
                            source_session: source.id.clone(),
                            source_revision: head.revision.revision,
                        },
                    }),
                None => None,
            };
            (
                SessionWorkRole::Primary,
                ActivationOrigin::Transfer {
                    source_session: source.id.clone(),
                },
                current_module_types(repositories, working_dir)?,
                initial,
            )
        }
        NewSessionWork::Child { preset, template } => (
            SessionWorkRole::Child,
            ActivationOrigin::Child {
                preset: preset.to_string(),
            },
            current_module_types(repositories, working_dir)?,
            template.map(|text| InitialRevision {
                text: text.to_string(),
                source: RevisionSource::Template {
                    preset: preset.to_string(),
                },
            }),
        ),
    };
    let workflow_path = surface.workflow_path(&session.id)?;
    let skill_path = SystemPromptComposer::from_repository_service(repositories.clone())
        .global_skill_path(SESSION_WORK_SKILL)?;
    let context_line = Notification::SessionWorkContext {
        workflow_path: &workflow_path.to_string_lossy(),
        skill_path: &skill_path.to_string_lossy(),
    }
    .render_with(repositories, working_dir)?;
    store.activate(
        &SessionWorkActivation {
            session: session.id.clone(),
            role,
            origin,
            activated_at: now,
            module_types,
        },
        initial.as_ref(),
    )?;
    surface.materialize(&session.id)?;
    session.session_work = Some(SessionWorkBinding {
        role,
        activated_at: now,
        context_line,
    });
    Ok(true)
}

/// Remove the session-work state of a session that was never published.
pub fn remove_unpublished(session: &str) -> anyhow::Result<()> {
    SessionWorkSurface::new()?.remove_unpublished(session)?;
    Ok(())
}

/// Bring an activated session's files back to its store at a safe point.
/// Returns the revision a changed file was restored to, if any.
pub fn reconcile_session(session: &Session) -> Result<Option<u32>, SessionWorkError> {
    if session.session_work.is_none() {
        return Ok(None);
    }
    let surface =
        SessionWorkSurface::new().map_err(|error| SessionWorkError::Io(error.to_string()))?;
    // The Session says it was activated; a store without that activation
    // (replaced or restored from elsewhere) is a contradiction, not "no
    // workflow".
    if surface.store().activation(&session.id)?.is_none() {
        return Err(SessionWorkError::NotActivated(session.id.clone()));
    }
    Ok(match surface.reconcile(&session.id)? {
        super::Reconciled::Restored { revision } => Some(revision),
        _ => None,
    })
}

#[cfg(test)]
#[path = "activation_tests.rs"]
mod tests;
