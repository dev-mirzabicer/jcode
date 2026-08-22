#[test]
fn test_openai_supports_codex_models() {
    let _guard = jcode_base::storage::lock_test_env();
    jcode_base::auth::codex::set_active_account_override(Some(
        "openai-supports-codex-models".to_string(),
    ));
    jcode_base::provider::populate_account_models(vec![
        "gpt-5.6-sol".to_string(),
        "gpt-5.1-codex".to_string(),
        "gpt-5.1-codex-mini".to_string(),
        "gpt-5.2-codex".to_string(),
    ]);

    let creds = CodexCredentials {
        access_token: "test".to_string(),
        refresh_token: String::new(),
        id_token: None,
        account_id: None,
        expires_at: None,
    };

    let provider = OpenAIProvider::new(creds);
    assert!(provider.available_models().contains(&"gpt-5.2-codex"));
    assert!(provider.available_models().contains(&"gpt-5.1-codex-mini"));
    assert!(provider.available_models().contains(&"gpt-5.6-sol"));

    provider.set_model("gpt-5.6-sol").unwrap();
    assert_eq!(provider.model(), "gpt-5.6-sol");

    provider.set_model("gpt-5.1-codex").unwrap();
    assert_eq!(provider.model(), "gpt-5.1-codex");

    provider.set_model("gpt-5.1-codex-mini").unwrap();
    assert_eq!(provider.model(), "gpt-5.1-codex-mini");

    jcode_base::auth::codex::set_active_account_override(None);
}

#[test]
fn test_openai_switching_models_include_dynamic_catalog_entries() {
    let _guard = jcode_base::storage::lock_test_env();
    let dynamic_model = "gpt-5.9-switching-test";
    jcode_base::auth::codex::set_active_account_override(Some("switching-test".to_string()));
    jcode_base::provider::populate_account_models(vec![
        "gpt-5.4".to_string(),
        dynamic_model.to_string(),
    ]);

    let provider = OpenAIProvider::new(CodexCredentials {
        access_token: "test".to_string(),
        refresh_token: String::new(),
        id_token: None,
        account_id: None,
        expires_at: None,
    });

    let models = provider.available_models_for_switching();
    assert!(models.contains(&"gpt-5.4".to_string()));
    assert!(models.contains(&dynamic_model.to_string()));

    jcode_base::auth::codex::set_active_account_override(None);
}

#[test]
fn test_chatgpt_web_model_bypasses_live_api_catalog() {
    let _guard = jcode_base::storage::lock_test_env();
    jcode_base::auth::codex::set_active_account_override(Some("web-model-test".to_string()));
    jcode_base::provider::populate_account_models(vec!["gpt-5.6-sol".to_string()]);
    let _model = EnvVarGuard::set("JCODE_OPENAI_MODEL", CHATGPT_WEB_MODEL);

    let provider = OpenAIProvider::new(CodexCredentials {
        access_token: "test".to_string(),
        refresh_token: String::new(),
        id_token: None,
        account_id: None,
        expires_at: None,
    });

    assert_eq!(provider.model(), CHATGPT_WEB_MODEL);
    assert_eq!(provider.transport().as_deref(), Some("browser"));
    assert!(
        provider
            .available_models_for_switching()
            .contains(&CHATGPT_WEB_MODEL.to_string())
    );
    provider.set_model("gpt-5.6-sol").unwrap();
    provider.set_model(CHATGPT_WEB_MODEL).unwrap();
    assert_eq!(provider.model(), CHATGPT_WEB_MODEL);

    jcode_base::auth::codex::set_active_account_override(None);
}

#[test]
fn test_chatgpt_browser_only_runtime_rejects_api_models() {
    let _guard = jcode_base::storage::lock_test_env();
    let provider = OpenAIProvider::new_browser_only();

    assert_eq!(provider.model(), CHATGPT_WEB_MODEL);
    assert_eq!(provider.available_models(), vec![CHATGPT_WEB_MODEL]);
    assert_eq!(
        provider.available_models_for_switching(),
        vec![CHATGPT_WEB_MODEL.to_string()]
    );
    assert_eq!(provider.available_transports(), vec!["browser"]);
    provider.set_transport("browser").unwrap();
    assert!(provider.set_transport("auto").is_err());
    let err = provider
        .set_model("gpt-5.6-sol")
        .expect_err("browser-only runtime must not expose API models");
    assert!(err.to_string().contains("OpenAI API credentials"));
}

#[test]
fn test_chatgpt_web_model_environment_override_is_trimmed() {
    let _guard = jcode_base::storage::lock_test_env();
    let _model = EnvVarGuard::set("JCODE_OPENAI_MODEL", "  gpt-5.6-pro[web]  ");
    let provider = OpenAIProvider::new(CodexCredentials {
        access_token: "test".to_string(),
        refresh_token: String::new(),
        id_token: None,
        account_id: None,
        expires_at: None,
    });
    assert_eq!(provider.model(), CHATGPT_WEB_MODEL);
}

#[test]
fn test_summarize_ws_input_counts_tool_outputs() {
    let items = vec![
        serde_json::json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "hello"}]
        }),
        serde_json::json!({
            "type": "function_call",
            "call_id": "call_1",
            "name": "bash",
            "arguments": "{}"
        }),
        serde_json::json!({
            "type": "function_call_output",
            "call_id": "call_1",
            "output": "ok"
        }),
        serde_json::json!({"type": "unknown"}),
    ];

    assert_eq!(
        summarize_ws_input(&items),
        WsInputStats {
            total_items: 4,
            message_items: 1,
            function_call_items: 1,
            function_call_output_items: 1,
            other_items: 1,
        }
    );
}

#[test]
fn test_persistent_ws_idle_policy_thresholds() {
    assert!(!persistent_ws_idle_needs_healthcheck(Duration::from_secs(
        5
    )));
    assert!(persistent_ws_idle_needs_healthcheck(Duration::from_secs(
        WEBSOCKET_PERSISTENT_HEALTHCHECK_IDLE_SECS
    )));

    // Default idle-reconnect window: reuse below threshold, reconnect at/above it.
    let default = WEBSOCKET_PERSISTENT_IDLE_RECONNECT_SECS_DEFAULT;
    assert!(!idle_requires_reconnect_with(
        Some(default),
        Duration::from_secs(default - 1)
    ));
    assert!(idle_requires_reconnect_with(
        Some(default),
        Duration::from_secs(default)
    ));

    // Disabled (None / env=0): never force a reconnect on idle alone.
    assert!(!idle_requires_reconnect_with(
        None,
        Duration::from_secs(u32::MAX as u64)
    ));
}

#[tokio::test]
async fn persistent_ws_response_idle_expiry_ignores_recent_ping_activity() {
    let (mut state, server) = test_persistent_ws_state().await;
    state.last_activity_at = Instant::now();
    state.last_response_completed_at =
        Instant::now() - Duration::from_secs(WEBSOCKET_PERSISTENT_IDLE_RECONNECT_SECS_DEFAULT);

    let outcome = ensure_persistent_ws_is_healthy(&mut state, false)
        .await
        .expect("idle policy should not require websocket I/O");
    assert!(matches!(
        outcome,
        PersistentWsHealth::Reconnect {
            reset_reason: "idle_reconnect",
            ..
        }
    ));
    server.abort();
}

#[tokio::test]
async fn persistent_ws_background_keepalive_round_trips_and_answers_server_ping() {
    let (mut state, server, ping_notify, pong_notify) =
        test_persistent_ws_state_with_ping_notify().await;
    state.last_activity_at =
        Instant::now() - Duration::from_secs(WEBSOCKET_PERSISTENT_HEALTHCHECK_IDLE_SECS + 1);
    let connection_started_at = state.connected_at;
    let persistent_ws = Arc::new(Mutex::new(Some(state)));

    let keepalive = spawn_persistent_ws_keepalive_with_interval(
        Arc::downgrade(&persistent_ws),
        connection_started_at,
        "gpt-test".to_string(),
        Duration::from_millis(5),
    );

    tokio::time::timeout(Duration::from_secs(1), ping_notify.notified())
        .await
        .expect("background keepalive should send a ping");
    tokio::time::timeout(Duration::from_secs(1), pong_notify.notified())
        .await
        .expect("background keepalive should answer a server ping");
    keepalive.abort();

    let mut guard = persistent_ws.lock().await;
    let state = guard.as_mut().expect("keepalive should retain the socket");
    assert!(
        !persistent_ws_idle_needs_healthcheck(state.last_activity_at.elapsed()),
        "successful background round trip should refresh socket activity"
    );
    drop(guard);

    *persistent_ws.lock().await = None;
    server.abort();
}

#[tokio::test]
#[allow(
    clippy::await_holding_lock,
    reason = "test intentionally serializes process-wide active OpenAI account model cache across async websocket state setup"
)]
async fn test_set_model_clears_persistent_ws_state() {
    let _guard = jcode_base::storage::lock_test_env();
    jcode_base::auth::codex::set_active_account_override(Some(
        "openai-set-model-clears-ws".to_string(),
    ));
    jcode_base::provider::populate_account_models(vec!["gpt-5.3-codex".to_string()]);

    let provider = OpenAIProvider::new(CodexCredentials {
        access_token: "test".to_string(),
        refresh_token: String::new(),
        id_token: None,
        account_id: None,
        expires_at: None,
    });
    let (state, server) = test_persistent_ws_state().await;
    *provider.persistent_ws.lock().await = Some(state);

    provider.set_model("gpt-5.3-codex").expect("set model");

    assert!(
        provider.persistent_ws.lock().await.is_none(),
        "changing models should reset the persistent websocket chain"
    );
    server.abort();
    jcode_base::auth::codex::set_active_account_override(None);
}

#[tokio::test]
async fn context_continuation_invalidation_forces_a_fresh_openai_request() {
    let provider = OpenAIProvider::new(CodexCredentials {
        access_token: "test".to_string(),
        refresh_token: String::new(),
        id_token: None,
        account_id: None,
        expires_at: None,
    });
    let (state, server) = test_persistent_ws_state().await;
    *provider.persistent_ws.lock().await = Some(state);

    provider.invalidate_context_continuation("context revision 7 applied");

    assert!(
        provider.persistent_ws.lock().await.is_none(),
        "historical context changes must discard previous_response_id and input-count state"
    );
    let (tx, _rx) = mpsc::channel(1);
    let result = try_persistent_ws_continuation(
        &provider.persistent_ws,
        &serde_json::json!({"model": "gpt-5.6-sol"}),
        &[serde_json::json!({
            "type": "message",
            "role": "user",
            "content": "complete projected history"
        })],
        1,
        &tx,
    )
    .await;
    assert!(matches!(result, PersistentWsResult::NotAvailable));
    server.abort();

    // A successful fresh full request establishes a new response baseline. Only
    // after that baseline exists may the runtime resume incremental delivery.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind continuation websocket listener");
    let addr = listener.local_addr().expect("continuation listener addr");
    let continuation_server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept continuation client");
        let mut ws = tokio_tungstenite::accept_async(stream)
            .await
            .expect("accept continuation websocket");
        let request = ws
            .next()
            .await
            .expect("receive continuation request")
            .expect("valid continuation request");
        let WsMessage::Text(request) = request else {
            panic!("expected text continuation request");
        };
        let request: serde_json::Value =
            serde_json::from_str(&request).expect("parse continuation request");
        assert_eq!(request["previous_response_id"], "resp_fresh_baseline");
        assert_eq!(
            request["input"].as_array().map(Vec::len),
            Some(1),
            "continuation after a fresh baseline must contain only the appended item"
        );
        assert_eq!(
            request["input"][0]["content"],
            "second after fresh baseline"
        );
        ws.send(WsMessage::Text(
            r#"{"type":"response.created","response":{"id":"resp_after_continuation"}}"#.into(),
        ))
        .await
        .expect("send response.created");
        ws.send(WsMessage::Text(
            r#"{"type":"response.completed","response":{"status":"completed"}}"#.into(),
        ))
        .await
        .expect("send response.completed");
    });
    let (client_ws, _) = connect_async(format!("ws://{addr}"))
        .await
        .expect("connect continuation websocket");
    *provider.persistent_ws.lock().await = Some(PersistentWsState {
        ws_stream: client_ws,
        last_response_id: "resp_fresh_baseline".to_string(),
        connected_at: Instant::now(),
        last_activity_at: Instant::now(),
        last_response_completed_at: Instant::now(),
        message_count: 1,
        last_input_item_count: 1,
    });
    let (tx, _rx) = mpsc::channel(8);
    let result = try_persistent_ws_continuation(
        &provider.persistent_ws,
        &serde_json::json!({"model": "gpt-5.6-sol"}),
        &[
            serde_json::json!({
                "type": "message",
                "role": "user",
                "content": "complete projected history"
            }),
            serde_json::json!({
                "type": "message",
                "role": "user",
                "content": "second after fresh baseline"
            }),
        ],
        2,
        &tx,
    )
    .await;
    assert!(matches!(result, PersistentWsResult::Success));
    {
        let guard = provider.persistent_ws.lock().await;
        let state = guard
            .as_ref()
            .expect("completed continuation remains reusable");
        assert_eq!(state.last_response_id, "resp_after_continuation");
        assert_eq!(state.last_input_item_count, 2);
    }
    continuation_server
        .await
        .expect("continuation websocket server");
}

#[tokio::test]
// The test-env lock is intentionally held across awaits to serialize
// process-global env access with other tests (see comment below).
#[allow(clippy::await_holding_lock)]
async fn test_switching_to_https_clears_persistent_ws_state() {
    // Serialize with the tests that set JCODE_OPENAI_MODEL via EnvVarGuard:
    // provider construction reads that process-global env var, so an
    // unsynchronized overlap can construct this provider pinned to the
    // browser-only web model and fail the HTTPS transport switch below.
    let _guard = jcode_base::storage::lock_test_env();
    let provider = OpenAIProvider::new(CodexCredentials {
        access_token: "test".to_string(),
        refresh_token: String::new(),
        id_token: None,
        account_id: None,
        expires_at: None,
    });
    let (state, server) = test_persistent_ws_state().await;
    *provider.persistent_ws.lock().await = Some(state);

    provider
        .set_transport("https")
        .expect("switch transport to https");

    assert!(
        provider.persistent_ws.lock().await.is_none(),
        "switching to HTTPS should drop the websocket continuation chain"
    );
    server.abort();
}

#[test]
fn test_service_tier_can_be_changed_while_a_request_snapshot_is_held() {
    let provider = Arc::new(OpenAIProvider::new(CodexCredentials {
        access_token: "test".to_string(),
        refresh_token: String::new(),
        id_token: None,
        account_id: None,
        expires_at: None,
    }));

    let read_guard = provider
        .service_tier
        .read()
        .expect("service tier read lock should be available");

    let (tx, rx) = std::sync::mpsc::channel();
    let provider_for_write = Arc::clone(&provider);
    let handle = std::thread::spawn(move || {
        let result = provider_for_write.set_service_tier("priority");
        tx.send(result).expect("send result from setter thread");
    });

    std::thread::sleep(Duration::from_millis(20));
    assert!(
        rx.try_recv().is_err(),
        "writer should wait for the in-flight snapshot to finish"
    );

    drop(read_guard);

    rx.recv()
        .expect("receive service tier setter result")
        .expect("service tier update should succeed once read lock is released");
    handle.join().expect("join setter thread");

    assert_eq!(provider.service_tier(), Some("priority".to_string()));
}

/// The OpenAI catalog endpoint and the chat endpoint must be selected by the
/// same authoritative discriminator: the loaded credential's *shape*
/// (`is_chatgpt_mode`), not the requested credential mode or a token-string
/// sniff. A platform API key (`sk-*`, no refresh/id token) must route to the
/// platform endpoints; a ChatGPT/Codex OAuth session must route to the Codex
/// endpoints. If these ever diverge, OpenAI returns 401.
#[test]
fn openai_catalog_and_chat_endpoints_agree_on_credential_shape() {
    // API-key-shaped credential: no refresh token, no id token.
    let api_key_creds = CodexCredentials {
        access_token: "sk-platform-key".to_string(),
        refresh_token: String::new(),
        id_token: None,
        account_id: None,
        expires_at: None,
    };
    assert!(
        !OpenAIProvider::is_chatgpt_mode(&api_key_creds),
        "platform API key must not be treated as ChatGPT/Codex mode"
    );
    assert!(
        OpenAIProvider::responses_url(&api_key_creds).starts_with(OPENAI_API_BASE),
        "platform API key chat requests must use the platform API base"
    );

    // OAuth-shaped credential: has a refresh token (Codex/ChatGPT session).
    let oauth_creds = CodexCredentials {
        access_token: "oauth-access".to_string(),
        refresh_token: "oauth-refresh".to_string(),
        id_token: None,
        account_id: None,
        expires_at: None,
    };
    assert!(
        OpenAIProvider::is_chatgpt_mode(&oauth_creds),
        "OAuth session with a refresh token must be treated as ChatGPT/Codex mode"
    );
    assert!(
        OpenAIProvider::responses_url(&oauth_creds).starts_with(CHATGPT_API_BASE),
        "OAuth chat requests must use the ChatGPT/Codex API base"
    );

    // An id-token-only credential is also a ChatGPT/Codex session.
    let id_token_creds = CodexCredentials {
        access_token: "oauth-access".to_string(),
        refresh_token: String::new(),
        id_token: Some("id-token".to_string()),
        account_id: None,
        expires_at: None,
    };
    assert!(
        OpenAIProvider::is_chatgpt_mode(&id_token_creds),
        "credential with an id token must be treated as ChatGPT/Codex mode"
    );
}

/// Issue #343: the native `openai-api` (Responses API) base URL must be
/// overridable for API-key usage so local/proxied Responses endpoints work,
/// while ChatGPT/Codex OAuth mode stays pinned to the Codex backend.
#[test]
fn responses_url_honors_api_base_override_in_api_key_mode() {
    let _guard = jcode_base::storage::lock_test_env();
    let _b = EnvVarGuard::remove("JCODE_OPENAI_API_BASE");
    let _c = EnvVarGuard::remove("OPENAI_BASE_URL");
    let _d = EnvVarGuard::remove("OPENAI_API_BASE");

    let api_key_creds = CodexCredentials {
        access_token: "sk-platform-key".to_string(),
        refresh_token: String::new(),
        id_token: None,
        account_id: None,
        expires_at: None,
    };

    // Default base when unset.
    assert_eq!(
        OpenAIProvider::responses_url(&api_key_creds),
        format!("{}/responses", OPENAI_API_BASE),
    );

    // Override is applied (and a trailing slash is tolerated).
    let _override = EnvVarGuard::set("JCODE_OPENAI_API_BASE", "http://127.0.0.1:8317/v1/");
    assert_eq!(
        OpenAIProvider::responses_url(&api_key_creds),
        "http://127.0.0.1:8317/v1/responses",
    );
    // WS URL derives from the same base.
    assert_eq!(
        OpenAIProvider::responses_ws_url(&api_key_creds),
        "ws://127.0.0.1:8317/v1/responses",
    );
}

#[test]
fn responses_url_ignores_override_in_chatgpt_mode() {
    let _guard = jcode_base::storage::lock_test_env();
    let _override = EnvVarGuard::set("JCODE_OPENAI_API_BASE", "http://127.0.0.1:8317/v1");

    let oauth_creds = CodexCredentials {
        access_token: "oauth-access".to_string(),
        refresh_token: "oauth-refresh".to_string(),
        id_token: None,
        account_id: None,
        expires_at: None,
    };
    // ChatGPT/Codex OAuth backend must stay fixed regardless of the override.
    assert!(
        OpenAIProvider::responses_url(&oauth_creds).starts_with(CHATGPT_API_BASE),
        "ChatGPT/Codex mode must ignore the API base override"
    );
}

#[test]
fn resolve_api_base_precedence_and_validation() {
    let _guard = jcode_base::storage::lock_test_env();
    let _a = EnvVarGuard::remove("JCODE_OPENAI_API_BASE");
    let _b = EnvVarGuard::remove("OPENAI_BASE_URL");
    let _c = EnvVarGuard::remove("OPENAI_API_BASE");

    // Default.
    assert_eq!(OpenAIProvider::resolve_api_base(), OPENAI_API_BASE);

    // JCODE_OPENAI_API_BASE wins over OPENAI_BASE_URL / OPENAI_API_BASE.
    let _p1 = EnvVarGuard::set("OPENAI_API_BASE", "https://c.example/v1");
    let _p2 = EnvVarGuard::set("OPENAI_BASE_URL", "https://b.example/v1");
    let _p3 = EnvVarGuard::set("JCODE_OPENAI_API_BASE", "https://a.example/v1");
    assert_eq!(OpenAIProvider::resolve_api_base(), "https://a.example/v1");

    // A trailing /responses is trimmed so callers don't double it.
    let _p4 = EnvVarGuard::set("JCODE_OPENAI_API_BASE", "https://a.example/v1/responses");
    assert_eq!(OpenAIProvider::resolve_api_base(), "https://a.example/v1");

    // Non-URL values are ignored, falling through to the next candidate.
    let _p5 = EnvVarGuard::set("JCODE_OPENAI_API_BASE", "not-a-url");
    assert_eq!(OpenAIProvider::resolve_api_base(), "https://b.example/v1");
}
