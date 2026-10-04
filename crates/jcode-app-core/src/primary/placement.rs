//! Placement for process-owned primaries (`jcode run`, `jcode repl`).
//!
//! Hosted sessions are placed through the runtime's `place` control after a
//! client review. A process-owned primary has no separate client, so its
//! caller opts in with `--place`: the proposed default placement is applied
//! through the same workspace owners and adoption control. A proposal without
//! a safe default (a broad root such as home) is refused, never guessed.
use super::*;
use crate::workspace::{
    LegacyLocationAdoptionRequest, LocationChangeState, RequestId, SessionPlacementRequest,
    WorkspaceService,
};
use anyhow::Result;

/// Place `agent`'s unplaced Session at its proposed default and return a
/// one-line description. Already placed or unrestricted Sessions are left as
/// they are.
pub async fn place_process_primary(agent: &mut Agent) -> Result<Option<String>> {
    if !agent.startup_context_session().requires_placement() {
        return Ok(None);
    }
    let workspace = WorkspaceService::new(&crate::storage::durable_state_dir());
    let session = agent.startup_context_session().clone();
    let proposal = workspace.propose_session_placement(&session)?;
    let Some(candidate) = proposal
        .default
        .and_then(|index| proposal.candidates.get(index))
    else {
        let roots = proposal
            .candidates
            .iter()
            .map(|candidate| candidate.root.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "No placement is proposed for {} (broad root: {roots}). Run from a project directory, or place the session in /workspace.",
            proposal.working_dir.display()
        );
    };
    let request = SessionPlacementRequest {
        request: RequestId::new(),
        session: session.id.clone(),
        working_dir: proposal.working_dir.clone(),
        expected_catalog_revision: proposal.catalog_revision,
        placement: candidate.placement.clone(),
    };
    let (placement, expected_catalog_revision) =
        workspace.resolve_session_placement(&request, &session)?;
    let record = workspace.request_legacy_adoption(LegacyLocationAdoptionRequest {
        request: request.request,
        session: session.id.clone(),
        expected_working_dir: Some(proposal.working_dir.clone()),
        expected_catalog_revision,
        placement,
        cwd: proposal.working_dir.clone(),
    })?;
    agent.apply_primary_location_changes().await?;
    let record = workspace.inspect_location_change(record.operation)?;
    ensure!(
        record.state == LocationChangeState::Complete
            && agent.startup_context_session().location.is_some(),
        "Placement did not complete ({:?}){}",
        record.state,
        record
            .issue
            .map(|issue| format!(": {}", issue.detail))
            .unwrap_or_default()
    );
    Ok(Some(format!(
        "{} at {}",
        candidate.name,
        candidate.root.display()
    )))
}

/// Refuse to start work in an unplaced Session before any input is recorded.
pub fn require_process_primary_placed(agent: &Agent) -> Result<()> {
    ensure!(
        !agent.startup_context_session().requires_placement(),
        "{} Add --place to use the proposed placement for this directory, or launch it from /workspace.",
        crate::workspace::PLACEMENT_REQUIRED
    );
    Ok(())
}
