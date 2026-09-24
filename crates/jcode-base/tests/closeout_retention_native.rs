//! Opt-in joined closeout evidence using the production read-only Session and
//! retained-output owners. Invoked only by an isolated owned native fixture.
use jcode_base::{
    execution::{ExecutionStore, inspection},
    session::Session,
};
use jcode_tool_types::execution::{ExecutionContent, ExecutionRequest};

#[tokio::test]
#[ignore = "requires owned closeout-retention fixture and exact Session/run identities"]
async fn closeout_native_capture_retained_session_and_outputs() {
    assert!(std::env::var_os("JCODE_TEST_STATE_ROOT").is_some());
    let root = std::path::PathBuf::from(std::env::var_os("JCODE_WP07_RETENTION_ROOT").unwrap());
    assert!(root.join("wp07-retention-owner.json").is_file());
    let home = std::path::PathBuf::from(std::env::var_os("JCODE_HOME").unwrap());
    assert_eq!(
        home.canonicalize().unwrap(),
        root.join("home").canonicalize().unwrap()
    );
    let label = std::env::var("JCODE_WP07_RETENTION_LABEL").unwrap();
    assert!(["before", "closed", "unavailable", "repaired"].contains(&label.as_str()));
    let intent: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("retention-intent.json")).unwrap())
            .unwrap();
    let session_id = intent["session"].as_str().unwrap();
    let captured = Session::capture_readonly(&home, session_id).unwrap();
    let store = ExecutionStore::open(&home).unwrap();
    let mut outputs = Vec::new();
    for id in intent["runs"].as_array().unwrap() {
        let id = id.as_str().unwrap();
        let run = store.inspect(id).unwrap().unwrap();
        assert_eq!(run.session_id, session_id);
        assert!(run.state.terminal() && run.complete);
        let output = inspection::inspect(
            &home,
            session_id,
            ExecutionRequest::Read {
                run_id: id.into(),
                content: ExecutionContent::Output,
                read_point: None,
                output_size: None,
            },
        )
        .await
        .unwrap();
        outputs.push(serde_json::json!({"run":run,"output":output}));
    }
    let result = serde_json::json!({
        "session":captured.session(),
        "outputs":outputs,
        "side_panel": jcode_base::side_panel::references_for_session_in(&home, session_id).unwrap(),
    });
    std::fs::write(
        root.join(format!("retention-{label}.json")),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
}
