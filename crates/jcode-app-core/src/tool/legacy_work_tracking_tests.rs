use super::*;
use crate::config::feature_override::ScopedFeatureOverride;
use jcode_message_types::ToolSetChange;

fn ctx(root: &std::path::Path, call: &str) -> ToolContext {
    ToolContext {
        session_id: "legacy-work-tracking".into(),
        message_id: "message".into(),
        tool_call_id: call.into(),
        working_dir: Some(root.into()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
        invocation: Default::default(),
    }
}

fn retained_rejection(error: anyhow::Error) -> String {
    let captured = error
        .downcast_ref::<crate::execution::CapturedToolError>()
        .expect("Registry rejection must be retained");
    let jcode_tool_types::OutputSource::Retained(reference) = &captured.output.source else {
        panic!("Retained rejection reference");
    };
    std::fs::read_to_string(&reference.path).unwrap()
}

#[tokio::test]
async fn legacy_work_tracking_initiative_is_absent_and_rejected_at_every_registry_path() {
    let home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    // The environment override wins over a saved `true`.
    std::fs::write(
        home.root().join("config.toml"),
        "[features]\nlegacy_work_tracking = true\n",
    )
    .unwrap();
    let _off = ScopedFeatureOverride::legacy_work_tracking(false);
    assert!(!crate::config::legacy_work_tracking_enabled());

    let registry = Registry::new(Arc::new(MockProvider)).await;
    assert!(!registry.tools.read().await.contains_key("initiative"));
    // A registry built before the gate closed cannot re-expose or run it.
    registry
        .tools
        .write()
        .await
        .insert("initiative".into(), Arc::new(goal::InitiativeTool::new()));
    assert!(
        !registry
            .tool_names()
            .await
            .iter()
            .any(|name| name == "initiative")
    );
    assert!(
        !registry
            .definitions(None)
            .await
            .iter()
            .any(|tool| tool.name == "initiative")
    );
    assert!(
        !registry
            .try_definitions(None)
            .unwrap()
            .iter()
            .any(|tool| tool.name == "initiative")
    );
    assert!(registry.input_binding_for("initiative").await.is_none());
    assert!(!tool_is_globally_available("initiative"));

    let error = registry
        .execute(
            "initiative",
            serde_json::json!({"action":"create","title":"must not persist","scope":"global"}),
            ctx(home.root(), "direct"),
        )
        .await
        .unwrap_err();
    assert_eq!(
        retained_rejection(error),
        format!(
            "\n[Execution error]\n{}",
            crate::config::LEGACY_WORK_TRACKING_UNAVAILABLE
        )
    );

    let output = registry
        .execute(
            "batch",
            serde_json::json!({"tool_calls":[
                {"tool":"initiative", "intent":"must reject", "action":"list"},
                {"tool":"initiative", "intent":"must reject", "action":"create", "title":"x"}
            ]}),
            ctx(home.root(), "batch"),
        )
        .await
        .unwrap();
    assert_eq!(
        output
            .output
            .matches(crate::config::LEGACY_WORK_TRACKING_UNAVAILABLE)
            .count(),
        2,
        "{}",
        output.output
    );

    // The tool itself never reached the store.
    assert!(!home.root().join("goals").exists());
    assert!(!home.root().join("memory").exists());
    let names = registry.tool_names().await;
    for retained in ["bash", "todo", "side_panel", "schedule"] {
        assert!(names.iter().any(|name| name == retained), "{retained}");
    }
}

#[tokio::test]
async fn legacy_work_tracking_enabled_registry_still_offers_initiative() {
    let _home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    let _on = ScopedFeatureOverride::legacy_work_tracking(true);
    let registry = Registry::new(Arc::new(MockProvider)).await;
    assert!(
        registry
            .definitions(None)
            .await
            .iter()
            .any(|tool| tool.name == "initiative")
    );
}

#[tokio::test]
async fn legacy_work_tracking_removal_is_one_announced_tool_set_change() {
    let _home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    // An existing session froze its tool set while initiative was offered.
    let frozen = {
        let _on = ScopedFeatureOverride::legacy_work_tracking(true);
        Registry::new(Arc::new(MockProvider))
            .await
            .definitions(None)
            .await
    };
    assert!(frozen.iter().any(|tool| tool.name == "initiative"));
    let first = tool_set::plan_tool_set(None, frozen.clone(), None, false);

    let _off = ScopedFeatureOverride::legacy_work_tracking(false);
    let live = Registry::new(Arc::new(MockProvider))
        .await
        .definitions(None)
        .await;
    assert!(!live.iter().any(|tool| tool.name == "initiative"));
    for inline in [false, true] {
        let in_view: Vec<ToolSetChange> = Vec::new();
        let inline_view = inline.then_some(in_view.as_slice());
        let second = tool_set::plan_tool_set(Some(&first.record), live.clone(), inline_view, false);
        assert_eq!(second.announced.len(), 1, "inline={inline}");
        assert!(matches!(
            &second.announced[0].change,
            ToolSetChange::Removed { name } if name == "initiative"
        ));
        assert!(second.record.is_withdrawn("initiative"), "inline={inline}");
        if inline {
            // In-message changes: the first-sent array stays, and the removal
            // inside the message names a tool it still declares.
            assert_eq!(second.tools, first.tools);
            assert!(!second.array_changed);
        } else {
            // Otherwise the definition leaves the array at one recorded
            // transition (Phase 4: never advertised).
            assert!(!second.tools.iter().any(|tool| tool.name == "initiative"));
            assert!(second.array_changed);
        }

        // The next request announces nothing and carries the same tools.
        let announced: Vec<ToolSetChange> = second
            .announced
            .iter()
            .map(|change| change.change.clone())
            .collect();
        let inline_view = inline.then_some(announced.as_slice());
        let third = tool_set::plan_tool_set(Some(&second.record), live.clone(), inline_view, false);
        assert!(third.announced.is_empty(), "inline={inline}");
        assert!(!third.array_changed, "inline={inline}");
        assert_eq!(third.tools, second.tools, "inline={inline}");
    }
}
