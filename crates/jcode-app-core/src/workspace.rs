//! Trusted same-user client adapter. No tool is registered by this module.
//! Catalog administration does not create a provisional inference Session.
pub use jcode_base::workspace::*;

pub async fn dispatch(request: WorkspaceRequest) -> WorkspaceResponse {
    let root = crate::storage::durable_state_dir();
    match tokio::task::spawn_blocking(move || dispatch_with(&WorkspaceService::new(&root), request))
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
pub fn dispatch_with(service: &WorkspaceService, request: WorkspaceRequest) -> WorkspaceResponse {
    use WorkspaceResponse as Response;
    let result = match request {
        WorkspaceRequest::Status => service.status().map(Response::Status),
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
        WorkspaceRequest::Snapshots => service.snapshots().map(Response::Snapshots),
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
