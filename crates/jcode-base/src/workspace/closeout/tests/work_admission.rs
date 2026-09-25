//! Work facts use their existing durable owners, not PID or prose guesses.
use super::*;

#[test]
fn pending_input_incoming_location_and_unresolved_execution_block_closeout() {
    let _environment = crate::storage::lock_test_env();
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
    let fixture = Fixture::new();
    let home = crate::storage::jcode_dir().unwrap();
    let mut waiting = crate::session::Session::create(None, None);
    waiting.working_dir = Some(fixture.root.canonicalize().unwrap().display().to_string());
    waiting.save().unwrap();
    let input:jcode_session_types::PrimaryInputEnvelope=serde_json::from_value(serde_json::json!({
        "id":RequestId::new(),"session":waiting.id,"delivery":"safe_boundary","content":"synthetic pending input","images":[]
    })).unwrap();
    let inputs =
        crate::primary_input::PrimaryInputStore::new(fixture.service.root.parent().unwrap());
    inputs.accept(input.clone()).unwrap();
    let mut incoming = crate::session::Session::create(None, None);
    incoming.working_dir = Some(
        fixture
            ._directory
            .path()
            .canonicalize()
            .unwrap()
            .display()
            .to_string(),
    );
    incoming.save().unwrap();
    let control = fixture
        .service
        .request_legacy_adoption(LegacyLocationAdoptionRequest {
            request: RequestId::new(),
            session: incoming.id.clone(),
            expected_working_dir: incoming.working_dir.as_deref().map(PathBuf::from),
            expected_catalog_revision: fixture.service.status().unwrap().revision,
            placement: Placement::Checkout(fixture.location),
            cwd: fixture.root.clone(),
        })
        .unwrap();
    let started = fixture.begin(false);
    let capture = capture(&fixture, started.operation);
    let execution =
        crate::execution::ExecutionStore::open(&fixture._directory.path().join("output")).unwrap();
    let invocation = crate::execution::Invocation {
        session_id: waiting.id.clone(),
        message_id: "prepared-native".into(),
        call_path: vec![RequestId::new().to_string()],
        tool: "bash".into(),
        input: serde_json::json!({"command":"synthetic fixture, never spawned"}),
        working_dir: Some(fixture.root.clone()),
        received_result_digest: None,
    };
    let crate::execution::PreparedInvocation::New(run) = execution
        .prepare(&invocation, "fixture-unresolved")
        .unwrap()
    else {
        panic!()
    };
    let (record, report) = fixture
        .service
        .prepare_closeout_work(
            started.operation,
            started.revision,
            &home,
            &execution,
            fixture._directory.path(),
            &capture,
        )
        .await
        .unwrap();
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.kind == CloseoutWorkKind::PendingInput
                && finding.identity == input.id.to_string()),
        "{report:?}"
    );
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.kind == CloseoutWorkKind::PendingControl
                && finding.identity == control.operation.to_string()),
        "{report:?}"
    );
    assert!(
        report.findings.iter().any(
            |finding| finding.kind == CloseoutWorkKind::Execution && finding.identity == run.id
        ),
        "{report:?}"
    );
    assert_eq!(
        inputs.inspect(&waiting.id, input.id).unwrap().state,
        jcode_session_types::PrimaryInputState::Accepted
    );
    assert_eq!(
        fixture
            .service
            .inspect_location_change(control.operation)
            .unwrap()
            .state,
        LocationChangeState::Pending
    );
    assert_eq!(
        execution.inspect(&run.id).unwrap().unwrap().state,
        crate::execution::RunState::Prepared
    );
    assert!(
        execution
            .inspect(&run.id)
            .unwrap()
            .unwrap()
            .stop_cause
            .is_none()
    );
    let damaged = home.join("sessions/fixture_unreadable.json");
    std::fs::write(&damaged, b"not a Session snapshot").unwrap();
    let (_, unknown) = fixture
        .service
        .prepare_closeout_work(
            started.operation,
            record.revision,
            &home,
            &execution,
            fixture._directory.path(),
            &capture,
        )
        .await
        .unwrap();
    assert!(
        unknown
            .findings
            .iter()
            .any(|finding| finding.kind == CloseoutWorkKind::Unknown
                && finding.identity == "fixture_unreadable"),
        "{unknown:?}"
    );
    std::fs::remove_file(damaged).unwrap();
    fixture
        .service
        .cancel_location_change(control.operation)
        .unwrap();
    // The metadata-only prepared producer never ran. Seal its owned fixture
    // receipt explicitly rather than leaving an ambiguous synthetic owner.
    execution.start(&run.id, "fixture-unresolved").unwrap();
    let run = execution.inspect(&run.id).unwrap().unwrap();
    let unfinished = crate::execution::Capture::create(execution, run, Default::default()).unwrap();
    finish_capture(&unfinished);
    finish_capture(&capture);
    });
}
