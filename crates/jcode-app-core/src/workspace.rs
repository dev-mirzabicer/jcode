//! Trusted same-user client adapter. No tool is registered by this module.
//! Catalog administration does not create a provisional inference Session.
pub use jcode_base::workspace::*;
mod clone_runner;
mod startup_copy;

pub(crate) async fn dispatch(
    request: WorkspaceRequest,
    client: String,
    coordinator: std::sync::Arc<crate::server::startup_context::StartupContextCoordinator>,
) -> WorkspaceResponse {
    if let WorkspaceRequest::BeginClone { request, review } = request {
        return clone_runner::begin(request, review, client).await;
    }
    if let WorkspaceRequest::ResumeClone { request } = request {
        return clone_runner::resume(request, client).await;
    }
    if let WorkspaceRequest::ApplyStartupCopy { request, review } = request {
        return startup_copy::apply(request, review, client, coordinator).await;
    }
    if let WorkspaceRequest::CloneOutput { clone, request } = request {
        return clone_runner::output(clone, request, client).await;
    }
    let root = crate::storage::durable_state_dir();
    match tokio::task::spawn_blocking(move || {
        match WorkspaceClientAuthority::authenticated(client) {
            Ok(client) => dispatch_with(&WorkspaceService::new(&root), request, &client),
            Err(error) => WorkspaceResponse::Error(error),
        }
    })
    .await
    {
        Ok(response) => response,
        Err(error) => WorkspaceResponse::Error(Issue {
            code: IssueCode::RecoveryRequired,
            detail: format!(
                "Catalog worker stopped without a reply. Inspect the durable request before retrying: {error}"
            ),
        }),
    }
}
pub fn dispatch_with(
    service: &WorkspaceService,
    request: WorkspaceRequest,
    client: &WorkspaceClientAuthority,
) -> WorkspaceResponse {
    use WorkspaceResponse as Response;
    let result = match request {
        WorkspaceRequest::Volumes {} => service.volumes().map(Response::Volumes),
        WorkspaceRequest::CloneOutput { .. } => Err(Issue {
            code: IssueCode::UnsupportedCapability,
            detail: "Clone output inspection requires the runtime execution reader".into(),
        }),
        WorkspaceRequest::ReviewClone {
            expected_revision,
            spec,
        } => service
            .review_clone(expected_revision, spec)
            .map(Response::CloneReview),
        WorkspaceRequest::InspectClone { request } => service
            .inspect_clone(request)
            .map(|record| Response::Clone(Box::new(record))),
        WorkspaceRequest::ReviewCloneTrust {
            clone,
            expected_revision,
        } => service
            .review_clone_trust(clone, expected_revision)
            .map(Response::CloneTrustReview),
        WorkspaceRequest::ApplyCloneTrust { request, review } => service
            .apply_clone_trust(request, review, client)
            .map(Response::Receipt),
        WorkspaceRequest::CancelClone { request } => service
            .request_clone_cancel(request)
            .map(|record| Response::Clone(Box::new(record))),
        WorkspaceRequest::InspectRebind { operation } => {
            service.inspect_rebind(operation).map(Response::Rebind)
        }
        WorkspaceRequest::ReviewStartupCopy {
            expected_catalog_revision,
            source,
            target,
            expected_source_plan_revision,
            expected_target_plan_revision,
            external_approvals,
        } => service
            .review_startup_copy(
                expected_catalog_revision,
                source,
                target,
                expected_source_plan_revision,
                expected_target_plan_revision,
                external_approvals,
            )
            .map(Response::StartupCopyReview),
        WorkspaceRequest::InspectStartupCopy { request } => service
            .inspect_startup_copy(request)
            .map(Response::StartupCopy),
        WorkspaceRequest::ApplyStartupCopy { .. } => Err(Issue {
            code: IssueCode::UnsupportedCapability,
            detail: "Startup Context copy requires the runtime-owned plan editor coordinator"
                .into(),
        }),
        WorkspaceRequest::BeginClone { .. } | WorkspaceRequest::ResumeClone { .. } => Err(Issue {
            code: IssueCode::UnsupportedCapability,
            detail:
                "A clone operation requires the runtime-owned asynchronous workspace dispatcher"
                    .into(),
        }),
        WorkspaceRequest::Permissions { request } => dispatch_permissions(service, request, client)
            .map(|value| Response::Permissions(Box::new(value))),
        WorkspaceRequest::Status {} => service.status().map(Response::Status),
        WorkspaceRequest::Initialize { request } => {
            service.initialize(request).map(Response::Status)
        }
        WorkspaceRequest::List {
            query,
            after,
            limit,
        } => service.list(query, after, limit).map(Response::Page),
        WorkspaceRequest::Inspect { target } => service.inspect(target).map(Response::Entity),
        WorkspaceRequest::Review {
            expected_revision,
            change,
        } => service
            .review_organization_change(expected_revision, change)
            .map(Response::Review),
        WorkspaceRequest::Apply { request, review } => service
            .apply_organization_change(request, review)
            .map(Response::Receipt),
        WorkspaceRequest::InspectReceipt { request } => {
            service.inspect_receipt(request).map(Response::Receipt)
        }
        WorkspaceRequest::Sessions {
            target,
            after,
            limit,
        } => service
            .sessions(target, after.as_deref(), limit)
            .map(Response::Sessions),
        WorkspaceRequest::Backup { request, name } => {
            service.backup(request, name).map(Response::Snapshot)
        }
        WorkspaceRequest::Snapshots {} => service.snapshots().map(Response::Snapshots),
        WorkspaceRequest::Export {
            request,
            project,
            name,
        } => service
            .export_project(request, project, name)
            .map(Response::Export),
        WorkspaceRequest::ReviewImport {
            path,
            expected_revision,
            collisions,
            remap,
        } => service
            .review_import(path, expected_revision, collisions, remap)
            .map(Response::ImportReview),
        WorkspaceRequest::ApplyImport { request, review } => {
            service.apply_import(request, review).map(Response::Receipt)
        }
        WorkspaceRequest::ReviewRestore { snapshot } => service
            .review_restore(snapshot)
            .map(Response::RestoreReview),
        WorkspaceRequest::ApplyRestore { request, review } => service
            .apply_restore(request, review)
            .map(Response::Receipt),
    };
    result.unwrap_or_else(Response::Error)
}

fn dispatch_permissions(
    service: &WorkspaceService,
    request: PermissionRequest,
    client: &WorkspaceClientAuthority,
) -> Result<PermissionResponse> {
    use PermissionResponse as Response;
    let load = |id: &str| {
        crate::session::Session::load_startup_stub(id).map_err(|error| Issue {
            code: IssueCode::RecoveryRequired,
            detail: format!("Read authoritative Session: {error:#}"),
        })
    };
    match request {
        PermissionRequest::DecideProposal {
            request,
            proposal,
            expected_revision,
            decision,
        } => service
            .decide_access_proposal(client, request, proposal, expected_revision, decision)
            .map(Response::Mutation),
        PermissionRequest::ImportedGrants { after, limit } => {
            service.list_imported_grants(after, limit)
        }
        PermissionRequest::AbandonContextScope { session } => {
            service.abandon_unpublished_context_scope(&session)?;
            service
                .context_scope_status_for_session(&session)
                .map(Response::ContextScopes)
        }
        PermissionRequest::ContextScopeStatus { review } => service
            .context_scope_status(review)
            .map(Response::ContextScopes),
        PermissionRequest::ReconcileContextScope { session } => {
            service.reconcile_context_scope(&session)?;
            let stored = load(&session)?;
            if let Some(creation) = stored.primary_creation {
                service.reconcile_primary_launch(creation.request)?;
            }
            service
                .context_scope_status_for_session(&session)
                .map(Response::ContextScopes)
        }
        PermissionRequest::ReviewCarry { session } => service
            .review_grant_carry(&session)
            .map(Response::CarryReview),
        PermissionRequest::Scope { session } => service
            .session_write_scope(&load(&session)?)
            .map(Response::Scope),
        PermissionRequest::Review {
            expected_revision,
            change,
        } => service
            .review_grant_change(expected_revision, change)
            .map(Response::Review),
        PermissionRequest::Apply { request, review } => service
            .apply_grant_change(client, request, review)
            .map(Response::Mutation),
        PermissionRequest::Grant { grant } => service.inspect_grant(grant).map(Response::Grant),
        PermissionRequest::Propose {
            session,
            request,
            target,
            reason,
        } => service
            .request_access(&load(&session)?, request, target, reason)
            .map(Response::Mutation),
        PermissionRequest::Proposal { proposal } => service
            .inspect_access_proposal(proposal)
            .map(Response::Proposal),
        PermissionRequest::List {
            query,
            after,
            limit,
        } => service
            .list_permissions(query, after, limit)
            .map(Response::Page),
    }
}
