//! Opt-in public SDK probe in a marker-owned native fixture. No model calls.
use jcode_sdk::{
    ConnectOptions, IndependentTasks, JcodeClient, RuntimeRequest, RuntimeResponse,
    ShutdownOptions, StopStrategy,
};

#[test]
#[ignore = "requires explicit WP08 owned fixture and bridge endpoint"]
fn runtime_native_sdk_fixture() -> Result<(), Box<dyn std::error::Error>> {
    let root =
        std::path::PathBuf::from(std::env::var_os("JCODE_WP08_NATIVE_ROOT").expect("fixture root"));
    assert!(root.join("owned-fixture.json").is_file());
    let socket = std::env::var_os("JCODE_WP08_API_SOCKET")
        .expect("fixture API socket")
        .into();
    let mode = std::env::var("JCODE_WP08_SDK_MODE")?;
    let client = JcodeClient::connect(ConnectOptions {
        socket_path: Some(socket),
        ensure_runtime: false,
        ..Default::default()
    });
    if mode == "stopped" {
        assert!(
            client
                .and_then(|client| client.runtime_control(RuntimeRequest::Status {}))
                .is_err()
        );
        return Ok(());
    }
    let client = client?;
    let response = match mode.as_str() {
        "review" => {
            let review = client.runtime_control(RuntimeRequest::Review {
                options: ShutdownOptions {
                    strategy: StopStrategy::FinishCurrent,
                    independent: IndependentTasks::KeepSupported,
                    quiescence_timeout_seconds: 10,
                    destination: Default::default(),
                },
            })?;
            let status = client.runtime_control(RuntimeRequest::Status {})?;
            std::fs::write(
                root.join("rust-runtime-status.json"),
                serde_json::to_vec_pretty(&status)?,
            )?;
            review
        }
        "inspect" => {
            let intent: serde_json::Value =
                serde_json::from_slice(&std::fs::read(root.join("sdk-runtime-intent.json"))?)?;
            client.runtime_control(RuntimeRequest::Inspect {
                operation: intent["operation"].as_str().unwrap().parse()?,
            })?
        }
        "begin" => {
            let review: RuntimeResponse =
                serde_json::from_slice(&std::fs::read(root.join("ts-runtime-review.json"))?)?;
            let RuntimeResponse::Review(review) = review else {
                return Err("Expected TS review".into());
            };
            let request = jcode_sdk::api::RequestId::new();
            std::fs::write(
                root.join("rust-runtime-intent.json"),
                serde_json::to_vec_pretty(
                    &serde_json::json!({"request":request,"review":review.id}),
                )?,
            )?;
            match client.runtime_control(RuntimeRequest::Begin {
                request,
                review: review.id,
            }) {
                Ok(response) => response,
                Err(error)
                    if matches!(
                        error.kind,
                        jcode_sdk::ErrorKind::Disconnected | jcode_sdk::ErrorKind::Timeout
                    ) =>
                {
                    std::fs::write(root.join("rust-runtime-uncertain.txt"), error.to_string())?;
                    return Ok(());
                }
                Err(error) => return Err(error.into()),
            }
        }
        _ => return Err("Unknown fixture mode".into()),
    };
    assert!(
        !matches!(response, RuntimeResponse::Error(_)),
        "{response:?}"
    );
    std::fs::write(
        root.join(format!("rust-runtime-{mode}.json")),
        serde_json::to_vec_pretty(&response)?,
    )?;
    Ok(())
}
