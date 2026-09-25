use super::*;
use crate::runtime_lifecycle::{
    RuntimeStopStore,
    admission::{RuntimeAdmission, scope},
};
use crate::workspace::runtime::*;

#[test]
#[cfg(target_os = "macos")]
fn stopping_runtime_allows_revocation_but_not_new_grant_authority() -> anyhow::Result<()> {
    let home = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    let checkout = tempfile::tempdir()?;
    let (service, location) =
        test_support::registered_checkout(&crate::storage::durable_state_dir(), checkout.path());
    let client = WorkspaceClientAuthority::authenticated("revocation-fixture")?;
    let issue = GrantChange::Issue {
        audience: Audience::Checkout(location),
        target: WriteTarget::Root(location),
        proposal: None,
    };
    let reviewed = service.review_grant_change(service.status()?.revision, issue.clone())?;
    service.apply_grant_change(&client, RequestId::new(), reviewed.id)?;
    let grant = reviewed.grant.id;
    let owner = RuntimeStopStore::new(
        &crate::storage::durable_state_dir(),
        &home.root().join("revocation.sock"),
    )?
    .claim()?;
    let registration = RuntimeAdmission::register(home.root(), owner.identity())?;
    let gate = registration.admission();
    let review = gate.review(
        &owner,
        ShutdownOptions {
            strategy: StopStrategy::Interrupt,
            independent: IndependentTasks::Stop,
            quiescence_timeout_seconds: 3,
        },
        Vec::new(),
    )?;
    let operation = gate.begin(&owner, RequestId::new(), review.id, Vec::new())?;
    let before = service.status()?.revision;
    assert!(matches!(
        dispatch_with(
            &service,
            WorkspaceRequest::Permissions {
                request: PermissionRequest::Review {
                    expected_revision: before,
                    change: issue
                }
            },
            &client
        ),
        WorkspaceResponse::Error(Issue {
            code: IssueCode::Busy,
            ..
        })
    ));
    let response = dispatch_with(
        &service,
        WorkspaceRequest::Permissions {
            request: PermissionRequest::Review {
                expected_revision: before,
                change: GrantChange::Revoke { grant },
            },
        },
        &client,
    );
    let WorkspaceResponse::Permissions(response) = response else {
        anyhow::bail!("Revocation review was not available: {response:?}");
    };
    let PermissionResponse::Review(review) = *response else {
        anyhow::bail!("Wrong revocation response");
    };
    assert!(matches!(
        dispatch_with(
            &service,
            WorkspaceRequest::Permissions {
                request: PermissionRequest::Apply {
                    request: RequestId::new(),
                    review: review.id
                }
            },
            &client
        ),
        WorkspaceResponse::Permissions(_)
    ));
    assert_eq!(service.inspect_grant(grant)?.state, GrantState::Revoked);
    gate.complete(&owner, operation.id, operation.revision)?;
    assert!(matches!(
        dispatch_with(
            &service,
            WorkspaceRequest::Permissions {
                request: PermissionRequest::Review {
                    expected_revision: service.status()?.revision,
                    change: GrantChange::Revoke { grant }
                }
            },
            &client
        ),
        WorkspaceResponse::Error(_)
    ));
    Ok(())
}

#[test]
fn shutdown_fences_workspace_effects_but_preserves_inspection_and_admitted_publication()
-> anyhow::Result<()> {
    let home = crate::auth::test_sandbox::AuthTestSandbox::new()?;
    let service = WorkspaceService::new(&crate::storage::durable_state_dir());
    service.initialize(RequestId::new())?;
    let client = WorkspaceClientAuthority::authenticated("fixture")?;
    let review = service.review_organization_change(
        service.status()?.revision,
        OrganizationChange::CreateProject {
            name: "admitted".into(),
        },
    )?;
    let owner = RuntimeStopStore::new(
        &crate::storage::durable_state_dir(),
        &home.root().join("catalog.sock"),
    )?
    .claim()?;
    let registration = RuntimeAdmission::register(home.root(), owner.identity())?;
    let gate = registration.admission();
    let held = gate.independent(
        RuntimeWorkKind::Preparation,
        "admitted-catalog".into(),
        None,
    )?;
    let stop_review = gate.review(
        &owner,
        ShutdownOptions {
            strategy: StopStrategy::FinishCurrent,
            independent: IndependentTasks::Stop,
            quiescence_timeout_seconds: 5,
        },
        Vec::new(),
    )?;
    let operation = gate.begin(&owner, RequestId::new(), stop_review.id, Vec::new())?;
    let before = service.status()?.revision;
    assert!(matches!(
        dispatch_with(
            &service,
            WorkspaceRequest::Apply {
                request: RequestId::new(),
                review: review.id
            },
            &client
        ),
        WorkspaceResponse::Error(Issue {
            code: IssueCode::Busy,
            ..
        })
    ));
    assert!(matches!(
        dispatch_with(&service, WorkspaceRequest::Status {}, &client),
        WorkspaceResponse::Status(_)
    ));
    assert!(matches!(
        dispatch_with(&service, WorkspaceRequest::Snapshots {}, &client),
        WorkspaceResponse::Snapshots(_)
    ));
    assert_eq!(service.status()?.revision, before);
    tokio::runtime::Runtime::new()?.block_on(async {
        let service = service.clone();
        let result = scope(Some(held), async move {
            crate::runtime_lifecycle::admission::spawn_blocking(move || {
                dispatch_with(
                    &service,
                    WorkspaceRequest::Apply {
                        request: RequestId::new(),
                        review: review.id,
                    },
                    &client,
                )
            })
            .await
        })
        .await?;
        assert!(matches!(result, WorkspaceResponse::Receipt(_)));
        Ok::<_, anyhow::Error>(())
    })?;
    assert!(gate.work()?.is_empty());
    assert!(service.status()?.revision > before);
    gate.cancel_wait(&owner, operation.id, operation.revision)?;
    Ok(())
}
