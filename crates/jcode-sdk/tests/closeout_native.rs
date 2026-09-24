//! Opt-in real-daemon probe. Invoked by scripts/test_workspace_closeout.py,
//! never as a substitute for an activated-runtime acceptance claim.
use jcode_sdk::{
    ConnectOptions, JcodeClient,
    api::{CloseoutReply, CloseoutRequest, CloseoutResponse},
};

#[test]
#[ignore = "requires an owned native fixture and its explicit endpoint/location"]
fn closeout_native_retained_history_after_removal() {
    assert!(std::env::var_os("JCODE_TEST_STATE_ROOT").is_some());
    let root = std::path::PathBuf::from(std::env::var_os("JCODE_WP07_NATIVE_ROOT").unwrap());
    assert!(root.join("wp07-owner.json").is_file());
    let socket = root.join("api.sock");
    let location = std::env::var("JCODE_WP07_LOCATION")
        .unwrap()
        .parse()
        .unwrap();
    let client = JcodeClient::connect(ConnectOptions {
        socket_path: Some(socket),
        ensure_runtime: false,
        ..Default::default()
    })
    .unwrap();
    let CloseoutReply::State { response } = client
        .closeout(CloseoutRequest::History { location })
        .unwrap()
    else {
        panic!("history rejected")
    };
    let CloseoutResponse::History(history) = *response else {
        panic!("wrong history reply")
    };
    assert_eq!(history.location.id, location);
    assert_eq!(
        history.record.as_ref().unwrap().stage,
        jcode_sdk::api::CloseoutStage::Closed
    );
    assert!(!history.location.observed_path.exists());
    assert!(history.report.as_ref().unwrap().is_file());
    std::fs::write(
        root.join("rust-sdk-history.json"),
        serde_json::to_vec_pretty(&history).unwrap(),
    )
    .unwrap();
}
