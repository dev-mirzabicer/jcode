//! Owned native fixtures shared by boundary tests. No production authority shortcut.
use super::*;

pub(crate) fn register_checkout(service: &WorkspaceService, path: &Path) -> LocationId {
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
        service,
        OrganizationChange::CreateProject {
            name: "fixture".into(),
        },
    ) else {
        panic!()
    };
    let EntityId::Repository(repository) = change(
        service,
        OrganizationChange::CreateRepository {
            name: "fixture".into(),
            remotes: vec![],
        },
    ) else {
        panic!()
    };
    change(
        service,
        OrganizationChange::AssociateRepository {
            project,
            repository,
        },
    );
    let EntityId::Location(location) = change(
        service,
        OrganizationChange::RegisterLocation {
            name: "fixture".into(),
            path: path.into(),
            registration: Registration::Checkout {
                home: Home::Project(project),
                repository,
            },
        },
    ) else {
        panic!()
    };
    location
}

pub(crate) async fn fence(service: &WorkspaceService, location: LocationId) -> CloseoutRecord {
    use crate::execution::{
        Capture, ExecutionStore, Invocation, PreparedInvocation, RunState, StorageConfig,
    };
    use jcode_tool_core::OutputCapture;
    let client = WorkspaceClientAuthority::authenticated("fixture-human").unwrap();
    let record = service
        .begin_closeout(
            &client,
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
    let state = service.root.parent().unwrap();
    let execution = ExecutionStore::open(&state.join("test-execution")).unwrap();
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
    let capture = Capture::create(execution.clone(), run, StorageConfig::default()).unwrap();
    let (record, _) = service
        .prepare_closeout_work(
            record.operation,
            record.revision,
            &state.join("sessions-fixture"),
            &execution,
            state,
            &capture,
        )
        .await
        .unwrap();
    let mut output = jcode_tool_types::ToolOutput::new("");
    output.source = jcode_tool_types::OutputSource::Retained(capture.reference().unwrap());
    capture.seal(output, RunState::Completed).unwrap();
    record
}
