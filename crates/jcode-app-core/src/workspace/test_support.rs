//! Disposable service fixtures for caller-admission checks.
use super::*;
use std::path::Path;

pub(crate) fn registered_checkout(state: &Path, checkout: &Path) -> (WorkspaceService, LocationId) {
    std::fs::create_dir_all(checkout).unwrap();
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@localhost",
            "commit",
            "--allow-empty",
            "-qm",
            "fixture",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .current_dir(checkout)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    std::fs::write(checkout.join("payload.md"), "retained synthetic data").unwrap();
    let service = WorkspaceService::new(state);
    service.initialize(RequestId::new()).unwrap();
    fn change(service: &WorkspaceService, change: OrganizationChange) -> EntityId {
        let review = service
            .review_organization_change(service.status().unwrap().revision, change)
            .unwrap();
        service
            .apply_organization_change(RequestId::new(), review.id)
            .unwrap()
            .targets[0]
    }
    let EntityId::Project(project) = change(
        &service,
        OrganizationChange::CreateProject {
            name: "fixture".into(),
        },
    ) else {
        panic!()
    };
    let EntityId::Repository(repository) = change(
        &service,
        OrganizationChange::CreateRepository {
            name: "fixture".into(),
            remotes: vec![],
        },
    ) else {
        panic!()
    };
    change(
        &service,
        OrganizationChange::AssociateRepository {
            project,
            repository,
        },
    );
    let EntityId::Location(location) = change(
        &service,
        OrganizationChange::RegisterLocation {
            name: "fixture".into(),
            path: checkout.into(),
            registration: Registration::Checkout {
                home: Home::Project(project),
                repository,
            },
        },
    ) else {
        panic!()
    };
    (service, location)
}

pub(crate) async fn closing_checkout(state: &Path, home: &Path, checkout: &Path) {
    let (service, location) = registered_checkout(state, checkout);
    closeout_work(&service, location, home, state).await;
}

pub(crate) async fn closeout_work(
    service: &WorkspaceService,
    location: LocationId,
    home: &Path,
    state: &Path,
) -> (CloseoutRecord, CloseoutWorkReport) {
    use crate::execution::{Capture, ExecutionStore, Invocation, PreparedInvocation, RunState};
    use jcode_tool_core::OutputCapture;
    let record = service
        .begin_closeout(
            &WorkspaceClientAuthority::authenticated("fixture-human").unwrap(),
            RequestId::new(),
            service.status().unwrap().revision,
            CloseoutSpec {
                location,
                expected_generation: 1,
                preservation_directory: None,
                conditional_no_loss: false,
                full_archive: false,
            },
        )
        .unwrap();
    let execution = ExecutionStore::open(home).unwrap();
    let invocation = Invocation {
        session_id: "workspace-fixture".into(),
        message_id: record.operation.to_string(),
        call_path: vec![RequestId::new().to_string()],
        tool: "closeout-fixture".into(),
        input: serde_json::json!({}),
        working_dir: Some(state.to_path_buf()),
        received_result_digest: None,
    };
    let PreparedInvocation::New(run) = execution.prepare(&invocation, "fixture").unwrap() else {
        panic!()
    };
    execution.start(&run.id, "fixture").unwrap();
    let capture = Capture::create(execution.clone(), run, Default::default()).unwrap();
    let (record, report) = service
        .prepare_closeout_work(
            record.operation,
            record.revision,
            home,
            &execution,
            state,
            &capture,
        )
        .await
        .unwrap();
    let mut output = jcode_tool_types::ToolOutput::new("");
    output.source = jcode_tool_types::OutputSource::Retained(capture.reference().unwrap());
    capture.seal(output, RunState::Completed).unwrap();
    (record, report)
}
