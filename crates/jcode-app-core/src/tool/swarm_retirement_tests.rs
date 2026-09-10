use super::*;

struct SwarmAvailabilityEnv(Option<std::ffi::OsString>);

impl SwarmAvailabilityEnv {
    fn disabled() -> Self {
        let previous = std::env::var_os("JCODE_SWARM_ENABLED");
        crate::env::set_var("JCODE_SWARM_ENABLED", "false");
        Self(previous)
    }
}

impl Drop for SwarmAvailabilityEnv {
    fn drop(&mut self) {
        if let Some(previous) = &self.0 {
            crate::env::set_var("JCODE_SWARM_ENABLED", previous);
        } else {
            crate::env::remove_var("JCODE_SWARM_ENABLED");
        }
    }
}

#[tokio::test]
async fn swarm_retirement_registry_direct_alias_batch_and_cached_definitions() {
    let home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let _availability = SwarmAvailabilityEnv::disabled();
    std::fs::write(home.root().join("config.toml"), "[features]\nswarm = true\n").unwrap();
    assert!(!crate::config::config().features.swarm, "global environment override must win over serialized true");
    let registry = Registry::new(Arc::new(MockProvider)).await;
    assert!(!registry.tools.read().await.contains_key("swarm"));
    // Represent a registry created before global disablement, without ever
    // enabling the live feature or executing an enabled Swarm operation.
    let direct = communicate::CommunicateTool::new();
    registry
        .tools
        .write()
        .await
        .insert("swarm".into(), Arc::new(direct));
    assert!(
        !registry
            .tool_names()
            .await
            .iter()
            .any(|name| name == "swarm")
    );
    assert!(
        !registry
            .definitions(None)
            .await
            .iter()
            .any(|tool| tool.name == "swarm")
    );
    assert!(
        !registry
            .try_definitions(None)
            .unwrap()
            .iter()
            .any(|tool| tool.name == "swarm")
    );
    let ctx = ToolContext {
        session_id: "swarm-retirement".into(),
        message_id: "message".into(),
        tool_call_id: "tool".into(),
        working_dir: Some(home.root().into()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    };
    for name in ["swarm", "communicate"] {
        // Only real aliases should be used here. The alias assertion ensures a
        // rejected unknown name cannot masquerade as enforcement evidence.
        assert_eq!(Registry::resolve_tool_name(name), "swarm");
        let error = registry
            .execute(
                name,
                serde_json::json!({"action":"spawn","label":"fixture"}),
                ctx.clone(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), crate::config::SWARM_UNAVAILABLE);
    }
    let error = communicate::CommunicateTool::new()
        .execute(serde_json::json!({"action":"run_plan"}), ctx.clone())
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), crate::config::SWARM_UNAVAILABLE);
    let output = registry
        .execute(
            "batch",
            serde_json::json!({"tool_calls":[
                {"tool":"swarm", "intent":"must reject", "action":"spawn", "label":"fixture"},
                {"tool":"communicate", "intent":"alias must reject", "action":"run_plan"}
            ]}),
            ctx.clone(),
        )
        .await
        .unwrap();
    assert!(output.output.contains(crate::config::SWARM_UNAVAILABLE));
    let nested = registry
        .execute(
            "batch",
            serde_json::json!({"tool_calls":[
                {"tool":"batch", "intent":"nested fixture", "tool_calls":[
                    {"tool":"swarm", "intent":"must never run", "action":"spawn", "label":"fixture"}
                ]}
            ]}),
            ctx,
        )
        .await
        .unwrap_err();
    assert!(nested.to_string().contains("Cannot batch"));
    assert!(!home.root().join("state/swarm").exists());
    let names = registry.tool_names().await;
    for retained in ["bash", "bg", "schedule", "selfdev"] {
        assert!(names.iter().any(|name| name == retained));
    }
}
