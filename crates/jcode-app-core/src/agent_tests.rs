use super::*;
use crate::agent::environment::EnvSnapshotDetail;
use crate::message::{Message, StreamEvent, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;
use crate::tool::ToolOutput;
use async_trait::async_trait;
use jcode_context_core::{build_content_target, build_message_range};
use jcode_provider_core::{
    ContextProjectionValidationOperation, ContextProjectionValidationReport, ContextProviderFamily,
    ContextProviderValidationIdentity, ContextReasoningBlockKind, ContextRequestBuilderValidation,
    context_projection_validation_report,
};
use jcode_session_types::{
    StoredContextArtifactGenerator, StoredContextAuthorization, StoredContextEmergencyAudit,
    StoredContextEmergencyOperationKind, StoredContextEmergencyPolicy,
    StoredContextEmergencyRetryOutcome, StoredContextEmergencyTriggerKind, StoredContextOperation,
    StoredContextStatusEvent, StoredContextTransaction, StoredContextTransactionStatusKind,
    StoredContextViewState, StoredRangeSummary, StoredReasoningSelection,
    StoredReasoningSuppression, StoredToolResultDistillation, StoredUnattendedContextAuthorization,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc as tokio_mpsc;
use tokio_stream::wrappers::ReceiverStream;

struct DelayedProvider {
    open_delay: Duration,
    first_event_delay: Duration,
}

struct ImmediateEmptyProvider;

#[derive(Clone)]
struct NoValidationSwitchProvider {
    model: Arc<std::sync::Mutex<String>>,
    invalidations: Arc<AtomicUsize>,
}

impl NoValidationSwitchProvider {
    fn new(model: &str) -> Self {
        Self {
            model: Arc::new(std::sync::Mutex::new(model.to_string())),
            invalidations: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
impl Provider for NoValidationSwitchProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }

    fn name(&self) -> &str {
        "no-validation-switch"
    }

    fn model(&self) -> String {
        self.model.lock().expect("model lock").clone()
    }

    fn set_model(&self, model: &str) -> Result<()> {
        *self.model.lock().expect("model lock") = model.to_string();
        Ok(())
    }

    fn invalidate_context_continuation(&self, _reason: &str) {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            model: Arc::new(std::sync::Mutex::new(self.model())),
            invalidations: Arc::clone(&self.invalidations),
        })
    }
}

#[derive(Clone)]
struct HiddenLimitEmergencyProvider {
    calls: Arc<AtomicUsize>,
    invalidations: Arc<AtomicUsize>,
    context_window: usize,
    fail_first_call: bool,
    reject_retry: bool,
    fail_retry_after_output: bool,
}

impl Default for HiddenLimitEmergencyProvider {
    fn default() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            invalidations: Arc::new(AtomicUsize::new(0)),
            context_window: 100_000,
            fail_first_call: true,
            reject_retry: false,
            fail_retry_after_output: false,
        }
    }
}

#[derive(Clone)]
struct ProjectedRequestProvider {
    state: Arc<ProjectedRequestProviderState>,
    context_window: usize,
}

#[derive(Default)]
struct ProjectedRequestProviderState {
    requests: std::sync::Mutex<Vec<Vec<Message>>>,
    invalidation_reasons: std::sync::Mutex<Vec<String>>,
    invalidations: AtomicUsize,
    summary_requests: AtomicUsize,
}

#[async_trait]
impl Provider for HiddenLimitEmergencyProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if (call == 0 && self.fail_first_call) || self.reject_retry {
            anyhow::bail!("maximum context length exceeded");
        }
        let (tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(4);
        let fail_after_output = self.fail_retry_after_output;
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(StreamEvent::TextDelta(
                    "recovered after one emergency transaction".to_string(),
                )))
                .await;
            if fail_after_output {
                let _ = tx
                    .send(Err(anyhow::anyhow!("retry stream failed after output")))
                    .await;
                return;
            }
            let _ = tx
                .send(Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }))
                .await;
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "openrouter"
    }

    fn model(&self) -> String {
        "hidden-limit-test".to_string()
    }

    fn context_window(&self) -> usize {
        self.context_window
    }

    fn validate_projected_context(
        &self,
        messages: &[Message],
        operations: &[ContextProjectionValidationOperation],
    ) -> ContextProjectionValidationReport {
        context_projection_validation_report(
            ContextProviderValidationIdentity {
                family: ContextProviderFamily::OpenRouterCompatible,
                provider_name: self.name().to_string(),
                provider_display_name: self.display_name(),
                model: self.model(),
                evidence_tag: "hidden_limit_emergency_test_v1".to_string(),
            },
            operations,
            Some(ContextReasoningBlockKind::GenericReasoning),
            Ok(ContextRequestBuilderValidation::new(messages.len())),
        )
    }

    fn invalidate_context_continuation(&self, _reason: &str) {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[derive(Clone)]
struct ScriptedSizeProvider {
    context_window: usize,
    open_error: Option<String>,
    stream_events: Arc<Vec<ScriptedSizeEvent>>,
    requests: Arc<AtomicUsize>,
}

#[derive(Clone)]
enum ScriptedSizeEvent {
    Event(StreamEvent),
}

impl ScriptedSizeProvider {
    fn open_error(context_window: usize, error: &str) -> Self {
        Self {
            context_window,
            open_error: Some(error.to_string()),
            stream_events: Arc::new(Vec::new()),
            requests: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn stream(context_window: usize, stream_events: Vec<ScriptedSizeEvent>) -> Self {
        Self {
            context_window,
            open_error: None,
            stream_events: Arc::new(stream_events),
            requests: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn request_count(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Provider for ScriptedSizeProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = self.open_error.as_ref() {
            anyhow::bail!(error.clone());
        }
        let (tx, rx) = tokio_mpsc::channel(16);
        let events = self.stream_events.clone();
        tokio::spawn(async move {
            for event in events.iter().cloned() {
                let result = match event {
                    ScriptedSizeEvent::Event(event) => Ok(event),
                };
                if tx.send(result).await.is_err() {
                    break;
                }
            }
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "scripted-size-test"
    }

    fn model(&self) -> String {
        "scripted-size-model".to_string()
    }

    fn context_window(&self) -> usize {
        self.context_window
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

impl ProjectedRequestProvider {
    fn new(context_window: usize) -> Self {
        Self {
            state: Arc::new(ProjectedRequestProviderState::default()),
            context_window,
        }
    }

    fn requests(&self) -> Vec<Vec<Message>> {
        self.state.requests.lock().unwrap().clone()
    }

    fn invalidation_count(&self) -> usize {
        self.state.invalidations.load(Ordering::SeqCst)
    }

    fn invalidation_reasons(&self) -> Vec<String> {
        self.state.invalidation_reasons.lock().unwrap().clone()
    }

    fn summary_request_count(&self) -> usize {
        self.state.summary_requests.load(Ordering::SeqCst)
    }
}

#[derive(Clone)]
struct SwitchableBudgetProvider {
    model: Arc<std::sync::Mutex<String>>,
}

impl SwitchableBudgetProvider {
    fn new(model: &str) -> Self {
        Self {
            model: Arc::new(std::sync::Mutex::new(model.to_string())),
        }
    }
}

#[async_trait]
impl Provider for ProjectedRequestProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        self.state.requests.lock().unwrap().push(messages.to_vec());
        let (tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(4);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(StreamEvent::TextDelta("projected response".to_string())))
                .await;
            let _ = tx
                .send(Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }))
                .await;
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "projected-request-test"
    }

    fn model(&self) -> String {
        "projected-request-model".to_string()
    }

    fn context_window(&self) -> usize {
        self.context_window
    }

    async fn complete_simple(&self, _prompt: &str, _system: &str) -> Result<String> {
        self.state.summary_requests.fetch_add(1, Ordering::SeqCst);
        Ok("legacy summary must not be requested".to_string())
    }

    fn invalidate_context_continuation(&self, reason: &str) {
        self.state.invalidations.fetch_add(1, Ordering::SeqCst);
        self.state
            .invalidation_reasons
            .lock()
            .unwrap()
            .push(reason.to_string());
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

fn context_test_timestamp() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-08-11T12:00:00Z")
        .expect("valid context test timestamp")
        .with_timezone(&chrono::Utc)
}

fn context_test_generator() -> StoredContextArtifactGenerator {
    StoredContextArtifactGenerator {
        provider: "test-provider".to_string(),
        model: "test-model".to_string(),
        route: "test-route".to_string(),
        prompt_version: "phase-5-projection-test".to_string(),
        effort: None,
        role: None,
        selection_source: None,
        transaction_instructions: None,
        task_instructions: None,
    }
}

fn context_test_message(id: &str, role: Role, content: Vec<ContentBlock>) -> StoredMessage {
    StoredMessage {
        id: id.to_string(),
        role,
        content,
        display_role: None,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: None,
    }
}

fn context_test_text(id: &str, role: Role, text: &str) -> StoredMessage {
    context_test_message(
        id,
        role,
        vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
    )
}

fn applied_context_state(operations: Vec<StoredContextOperation>) -> StoredContextViewState {
    StoredContextViewState {
        revision: 1,
        transactions: vec![context_test_transaction(
            "phase-5-context-transaction",
            0,
            1,
            operations,
        )],
        ..StoredContextViewState::default()
    }
}

fn context_test_transaction(
    id: &str,
    base_revision: u64,
    applied_revision: u64,
    operations: Vec<StoredContextOperation>,
) -> StoredContextTransaction {
    StoredContextTransaction {
        id: id.to_string(),
        base_revision,
        created_at: context_test_timestamp(),
        authorization: StoredContextAuthorization::Manual { initiated_by: None },
        operations,
        status_events: vec![StoredContextStatusEvent {
            revision: applied_revision,
            timestamp: context_test_timestamp(),
            kind: StoredContextTransactionStatusKind::Applied,
            reason: Some("projected request test".to_string()),
        }],
        application: None,
        economics: None,
        curator_usage: Vec::new(),
        emergency_audit: None,
    }
}

fn range_summary_operation(
    messages: &[StoredMessage],
    start: usize,
    end: usize,
    summary_text: &str,
) -> StoredContextOperation {
    StoredContextOperation::RangeSummary(StoredRangeSummary {
        source_range: build_message_range(messages, start, end).expect("valid summary range"),
        summary_text: summary_text.to_string(),
        file_change_digest: "No changed files in fixture".to_string(),
        changed_files: Vec::new(),
        change_evidence_complete: true,
        boundary_expansions: Vec::new(),
        generator: Some(context_test_generator()),
        source_token_estimate: 1_000,
        replacement_token_estimate: 100,
        warnings: Vec::new(),
        created_at: context_test_timestamp(),
        legacy_coverage: None,
    })
}

fn reasoning_suppression_operation(
    messages: &[StoredMessage],
    message_index: usize,
    block_index: usize,
) -> StoredContextOperation {
    let target =
        build_content_target(messages, message_index, block_index).expect("valid reasoning target");
    StoredContextOperation::ReasoningSuppression(StoredReasoningSuppression {
        selection: StoredReasoningSelection::MessageRanges {
            ranges: vec![
                build_message_range(messages, message_index, message_index)
                    .expect("valid reasoning range"),
            ],
        },
        targets: vec![target.clone()],
        assistant_turns_affected: 1,
        replay_block_kinds: vec![target.kind],
        original_token_estimate: 100,
        validation_evidence_version: 1,
        validation: Vec::new(),
    })
}

fn tool_distillation_operation(
    messages: &[StoredMessage],
    message_index: usize,
    block_index: usize,
    replacement_content: &str,
) -> StoredContextOperation {
    StoredContextOperation::ToolResultDistillation(StoredToolResultDistillation {
        target: build_content_target(messages, message_index, block_index)
            .expect("valid tool-result target"),
        tool_name: "bash".to_string(),
        tool_call_id: "phase-5-tool".to_string(),
        replacement_content: replacement_content.to_string(),
        original_token_estimate: 100,
        replacement_token_estimate: 10,
        replacement_ratio_millionths: 100_000,
        preservation_rationale: "Exact fixture facts preserved".to_string(),
        uncertainties: Vec::new(),
        generator: context_test_generator(),
        created_at: context_test_timestamp(),
    })
}

fn combined_projected_session() -> Session {
    let mut session = Session::create(None, None);
    session.append_stored_message(context_test_text(
        "summary-user",
        Role::User,
        "source range user text",
    ));
    session.append_stored_message(context_test_text(
        "summary-assistant",
        Role::Assistant,
        "source range assistant text",
    ));
    session.append_stored_message(context_test_message(
        "reasoning-message",
        Role::Assistant,
        vec![
            ContentBlock::Reasoning {
                text: "targeted replay reasoning".to_string(),
            },
            ContentBlock::Text {
                text: "visible reasoning answer".to_string(),
                cache_control: None,
            },
        ],
    ));
    session.append_stored_message(context_test_message(
        "tool-call-message",
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "phase-5-tool".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({"command": "printf fixture"}),
            thought_signature: Some("preserved-thought-signature".to_string()),
        }],
    ));
    session.append_stored_message(context_test_message(
        "tool-result-message",
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: "phase-5-tool".to_string(),
            content: "large original tool output with exact facts".repeat(20),
            is_error: Some(true),
        }],
    ));
    session.append_stored_message(context_test_text(
        "tail-message",
        Role::User,
        "unmodified tail",
    ));
    session.context_view = applied_context_state(vec![
        range_summary_operation(&session.messages, 0, 1, "selected historical summary"),
        reasoning_suppression_operation(&session.messages, 2, 0),
        tool_distillation_operation(&session.messages, 4, 0, "distilled exact tool facts"),
    ]);
    session
}

#[derive(Clone)]
struct ExplicitPinProvider {
    model: Arc<std::sync::Mutex<String>>,
    pin: Arc<std::sync::Mutex<Option<String>>>,
    set_model_requests: Arc<std::sync::Mutex<Vec<String>>>,
}

impl ExplicitPinProvider {
    fn new(model: &str) -> Self {
        Self {
            model: Arc::new(std::sync::Mutex::new(model.to_string())),
            pin: Arc::new(std::sync::Mutex::new(None)),
            set_model_requests: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl Provider for ExplicitPinProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        unreachable!("ExplicitPinProvider does not complete requests")
    }

    fn name(&self) -> &str {
        "openrouter"
    }

    fn model(&self) -> String {
        self.model.lock().unwrap().clone()
    }

    fn set_model(&self, request: &str) -> Result<()> {
        self.set_model_requests
            .lock()
            .unwrap()
            .push(request.to_string());
        let spec = request.strip_prefix("openrouter:").unwrap_or(request);
        let (model, pin) = spec
            .rsplit_once('@')
            .map(|(model, pin)| (model, Some(pin.to_string())))
            .unwrap_or((spec, None));
        *self.model.lock().unwrap() = model.to_string();
        *self.pin.lock().unwrap() = pin;
        Ok(())
    }

    fn explicit_provider_pin_for_current_model(&self) -> Option<String> {
        self.pin.lock().unwrap().clone()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[async_trait]
impl Provider for SwitchableBudgetProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        unreachable!("SwitchableBudgetProvider does not complete requests")
    }

    fn name(&self) -> &str {
        "switchable-budget"
    }

    fn model(&self) -> String {
        self.model.lock().unwrap().clone()
    }

    fn set_model(&self, request: &str) -> Result<()> {
        *self.model.lock().unwrap() = request.to_string();
        Ok(())
    }

    fn context_window(&self) -> usize {
        match self.model.lock().unwrap().as_str() {
            "large" => 50_000,
            _ => 10_000,
        }
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

async fn agent_context_budget_stats(agent: &Agent) -> crate::context_budget::ContextBudgetStats {
    let context_budget = agent.registry.context_budget();
    context_budget.read().await.stats()
}

fn estimated_message_tokens(messages: &[Message]) -> usize {
    let mut tracker = crate::context_budget::ContextBudgetTracker::new().with_budget(1);
    tracker.seed_messages(messages);
    tracker.estimated_message_tokens()
}

fn estimated_content_tokens(content: Vec<ContentBlock>) -> usize {
    estimated_message_tokens(&[Message {
        role: Role::User,
        content,
        timestamp: None,
        tool_duration_ms: None,
    }])
}

fn content_text(content: &[ContentBlock]) -> &str {
    match content.first() {
        Some(ContentBlock::Text { text, .. }) => text,
        _ => "",
    }
}

fn message_text(message: &Message) -> &str {
    content_text(&message.content)
}

#[async_trait]
impl Provider for DelayedProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        tokio::time::sleep(self.open_delay).await;

        let first_event_delay = self.first_event_delay;
        let (tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(8);
        tokio::spawn(async move {
            tokio::time::sleep(first_event_delay).await;
            let _ = tx
                .send(Ok(StreamEvent::TextDelta("hello".to_string())))
                .await;
            let _ = tx
                .send(Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }))
                .await;
        });

        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "delayed"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            open_delay: self.open_delay,
            first_event_delay: self.first_event_delay,
        })
    }
}

#[async_trait]
impl Provider for ImmediateEmptyProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let (_tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(1);
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "openai"
    }

    fn context_window(&self) -> usize {
        1_000
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }

    async fn complete_simple(&self, _prompt: &str, _system: &str) -> Result<String> {
        Ok("manual summary from native-auto provider".to_string())
    }
}

#[test]
fn tool_output_to_content_blocks_preserves_labeled_images() {
    let output = ToolOutput::new("Image ready").with_labeled_image(
        "image/png",
        "ZmFrZQ==",
        "screenshots/example.png",
    );

    let blocks = tool_output_to_content_blocks("call_1".to_string(), output);
    assert_eq!(blocks.len(), 3);

    match &blocks[0] {
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            assert_eq!(tool_use_id, "call_1");
            assert_eq!(content, "Image ready");
            assert_eq!(*is_error, None);
        }
        other => panic!("expected tool result, got {other:?}"),
    }

    match &blocks[1] {
        ContentBlock::Image { media_type, data } => {
            assert_eq!(media_type, "image/png");
            assert_eq!(data, "ZmFrZQ==");
        }
        other => panic!("expected image block, got {other:?}"),
    }

    match &blocks[2] {
        ContentBlock::Text { text, .. } => {
            assert!(text.contains("screenshots/example.png"));
            assert!(text.contains("preceding tool result"));
        }
        other => panic!("expected trailing label text, got {other:?}"),
    }
}

#[tokio::test]
async fn queued_soft_interrupt_images_are_injected_as_image_blocks() {
    let provider: Arc<dyn Provider> = Arc::new(ImmediateEmptyProvider);
    let registry = Registry::new(provider.clone()).await;
    let _guard = crate::storage::lock_test_env();
    let mut agent = Agent::new(provider, registry);

    agent.queue_soft_interrupt(
        "look at this".to_string(),
        vec![("image/png".to_string(), "ZmFrZQ==".to_string())],
        false,
        SoftInterruptSource::User,
    );
    let injected = agent.inject_soft_interrupts();

    assert_eq!(injected.len(), 1);
    let message = agent
        .session
        .messages
        .last()
        .expect("soft interrupt should append a user message");
    assert!(matches!(
        &message.content[0],
        ContentBlock::Image { media_type, data }
            if media_type == "image/png" && data == "ZmFrZQ=="
    ));
    assert!(matches!(
        &message.content[1],
        ContentBlock::Text { text, .. } if text == "look at this"
    ));
}

#[tokio::test]
async fn run_turn_streaming_mpsc_emits_keepalive_while_provider_is_quiet() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(DelayedProvider {
        open_delay: Duration::from_secs(2),
        first_event_delay: Duration::from_secs(2),
    });
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "test".to_string(),
            cache_control: None,
        }],
    );

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(tx).await });

    let mut saw_keepalive = false;
    let keepalive_deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < keepalive_deadline {
        match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
            Ok(Some(ServerEvent::Pong { id })) => {
                assert_eq!(id, STREAM_KEEPALIVE_PONG_ID);
                saw_keepalive = true;
                break;
            }
            Ok(Some(ServerEvent::TextDelta { text })) => {
                panic!("expected keepalive before text delta, got: {text}");
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("channel closed before keepalive"),
            Err(_) => {
                assert!(
                    !task.is_finished(),
                    "streaming task finished before keepalive arrived"
                );
            }
        }
    }
    assert!(saw_keepalive, "expected keepalive before provider response");

    let mut saw_text = false;
    let text_deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < text_deadline {
        match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
            Ok(Some(ServerEvent::TextDelta { text })) => {
                assert_eq!(text, "hello");
                saw_text = true;
                break;
            }
            Ok(Some(ServerEvent::Pong { id })) => {
                assert_eq!(id, STREAM_KEEPALIVE_PONG_ID);
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("channel closed before text delta"),
            Err(_) => {
                assert!(
                    !task.is_finished(),
                    "streaming task finished before text delta arrived"
                );
            }
        }
    }

    assert!(saw_text, "expected delayed provider text after keepalive");
    task.await.unwrap().unwrap();
}

/// Provider that transparently switches its model mid-stream, mimicking the
/// Anthropic retired-model fallback (`claude-fable-5` -> `claude-opus-4-8`).
struct MidStreamModelSwitchProvider {
    model: std::sync::Mutex<String>,
    switch_to: String,
}

#[async_trait]
impl Provider for MidStreamModelSwitchProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        // Emulate the provider switching its own model state during the request.
        *self.model.lock().unwrap() = self.switch_to.clone();
        let (tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(8);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(StreamEvent::TextDelta("hello".to_string())))
                .await;
            let _ = tx
                .send(Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }))
                .await;
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "claude"
    }

    fn model(&self) -> String {
        self.model.lock().unwrap().clone()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            model: std::sync::Mutex::new(self.model.lock().unwrap().clone()),
            switch_to: self.switch_to.clone(),
        })
    }
}

#[tokio::test]
async fn run_turn_streaming_mpsc_emits_model_changed_on_midstream_switch() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(MidStreamModelSwitchProvider {
        model: std::sync::Mutex::new("claude-fable-5".to_string()),
        switch_to: "claude-opus-4-8".to_string(),
    });
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "test".to_string(),
            cache_control: None,
        }],
    );

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(tx).await });

    let mut switched_model = None;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
            Ok(Some(ServerEvent::ModelChanged { model, error, .. })) => {
                assert!(error.is_none(), "unexpected model-change error: {error:?}");
                switched_model = Some(model);
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {
                if task.is_finished() {
                    break;
                }
            }
        }
    }

    task.await.unwrap().unwrap();
    assert_eq!(
        switched_model.as_deref(),
        Some("claude-opus-4-8"),
        "expected a ModelChanged event resyncing to the served model"
    );
}

#[test]
fn projected_request_defaults_to_raw_history_without_mutating_transcript() {
    let provider = Arc::new(ProjectedRequestProvider::new(10_000));
    let provider_dyn: Arc<dyn Provider> = provider;
    let mut session = Session::create(None, None);
    session.append_stored_message(context_test_text("default-user", Role::User, "hello"));
    session.append_stored_message(context_test_text(
        "default-assistant",
        Role::Assistant,
        "world",
    ));
    let mut agent = Agent::new_with_session(provider_dyn, Registry::empty(), session, None);
    let raw_before = serde_json::to_vec(&agent.session.messages).unwrap();
    let raw_provider = agent.session.raw_messages_for_provider_uncached();

    let projected = agent.messages_for_provider().expect("default projection");

    assert_eq!(
        serde_json::to_vec(&projected).unwrap(),
        serde_json::to_vec(&raw_provider).unwrap()
    );
    assert_eq!(
        serde_json::to_vec(&agent.session.messages).unwrap(),
        raw_before
    );
}

#[test]
fn projected_request_applies_selected_summary_at_the_original_position() {
    let provider: Arc<dyn Provider> = Arc::new(ProjectedRequestProvider::new(10_000));
    let mut session = Session::create(None, None);
    session.append_stored_message(context_test_text("prefix", Role::User, "prefix remains"));
    session.append_stored_message(context_test_text(
        "summary-source-user",
        Role::User,
        "selected user source",
    ));
    session.append_stored_message(context_test_text(
        "summary-source-assistant",
        Role::Assistant,
        "selected assistant source",
    ));
    session.append_stored_message(context_test_text("suffix", Role::User, "suffix remains"));
    session.context_view = applied_context_state(vec![range_summary_operation(
        &session.messages,
        1,
        2,
        "middle range summary",
    )]);
    let mut agent = Agent::new_with_session(provider, Registry::empty(), session, None);
    let raw_before = serde_json::to_vec(&agent.session.messages).unwrap();

    let projected = agent.messages_for_provider().expect("summary projection");
    let encoded = serde_json::to_string(&projected).unwrap();

    assert!(encoded.contains("prefix remains"));
    assert!(encoded.contains("middle range summary"));
    assert!(encoded.contains("suffix remains"));
    assert!(!encoded.contains("selected user source"));
    assert!(!encoded.contains("selected assistant source"));
    assert_eq!(
        serde_json::to_vec(&agent.session.messages).unwrap(),
        raw_before
    );
}

#[test]
fn projected_request_suppresses_only_the_targeted_replayed_reasoning() {
    let provider: Arc<dyn Provider> = Arc::new(ProjectedRequestProvider::new(10_000));
    let mut session = Session::create(None, None);
    session.append_stored_message(context_test_text("reason-user", Role::User, "question"));
    session.append_stored_message(context_test_message(
        "targeted-reasoning",
        Role::Assistant,
        vec![
            ContentBlock::Reasoning {
                text: "remove this replay reasoning".to_string(),
            },
            ContentBlock::Text {
                text: "targeted visible answer".to_string(),
                cache_control: None,
            },
        ],
    ));
    session.append_stored_message(context_test_message(
        "retained-reasoning",
        Role::Assistant,
        vec![
            ContentBlock::Reasoning {
                text: "retain this replay reasoning".to_string(),
            },
            ContentBlock::Text {
                text: "retained visible answer".to_string(),
                cache_control: None,
            },
        ],
    ));
    session.context_view = applied_context_state(vec![reasoning_suppression_operation(
        &session.messages,
        1,
        0,
    )]);
    let mut agent = Agent::new_with_session(provider, Registry::empty(), session, None);

    let projected = agent.messages_for_provider().expect("reasoning projection");
    let encoded = serde_json::to_string(&projected).unwrap();

    assert!(!encoded.contains("remove this replay reasoning"));
    assert!(encoded.contains("targeted visible answer"));
    assert!(encoded.contains("retain this replay reasoning"));
    assert!(encoded.contains("retained visible answer"));
}

#[test]
fn projected_request_distills_tool_result_without_changing_pair_metadata() {
    let provider: Arc<dyn Provider> = Arc::new(ProjectedRequestProvider::new(10_000));
    let mut session = Session::create(None, None);
    session.append_stored_message(context_test_message(
        "distill-call",
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "phase-5-tool".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({"command": "false"}),
            thought_signature: Some("signature-must-survive".to_string()),
        }],
    ));
    session.append_stored_message(context_test_message(
        "distill-result",
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: "phase-5-tool".to_string(),
            content: "original large result".repeat(30),
            is_error: Some(true),
        }],
    ));
    session.context_view = applied_context_state(vec![tool_distillation_operation(
        &session.messages,
        1,
        0,
        "distilled result",
    )]);
    let mut agent = Agent::new_with_session(provider, Registry::empty(), session, None);

    let projected = agent.messages_for_provider().expect("distilled projection");
    let tool_use = projected
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|block| match block {
            ContentBlock::ToolUse {
                id,
                thought_signature,
                ..
            } => Some((id, thought_signature)),
            _ => None,
        })
        .expect("projected tool use");
    let tool_result = projected
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Some((tool_use_id, content, is_error)),
            _ => None,
        })
        .expect("projected tool result");

    assert_eq!(tool_use.0, "phase-5-tool");
    assert_eq!(tool_use.1.as_deref(), Some("signature-must-survive"));
    assert_eq!(tool_result.0, "phase-5-tool");
    assert_eq!(tool_result.1, "distilled result");
    assert_eq!(*tool_result.2, Some(true));
}

#[tokio::test]
async fn live_agent_request_matches_combined_projection_without_automatic_context_mutation() {
    let _guard = crate::storage::lock_test_env();
    let provider = Arc::new(ProjectedRequestProvider::new(1_000));
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let mut agent = Agent::new_with_session(
        provider_dyn,
        Registry::empty(),
        combined_projected_session(),
        None,
    );
    agent.set_memory_enabled(false);
    let raw_message_count_before = agent.session.messages.len();
    let raw_before = serde_json::to_vec(&agent.session.messages).unwrap();
    let expected =
        jcode_context_core::project_context(&agent.session.messages, &agent.session.context_view)
            .expect("pure-core projection")
            .messages;
    let expected_stats = agent_context_budget_stats(&agent).await;
    assert_eq!(expected_stats.message_count, expected.len());
    assert_eq!(
        expected_stats.estimated_message_tokens,
        estimated_message_tokens(&expected)
    );

    let invalidations_before_request = provider.invalidation_count();

    let response = agent.run_turn(false).await.expect("live projected turn");

    assert_eq!(response, "projected response");
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        crate::message::cache_relevant_messages(&requests[0]),
        crate::message::cache_relevant_messages(&expected)
    );
    assert_eq!(provider.summary_request_count(), 0);
    assert_eq!(provider.invalidation_count(), invalidations_before_request);
    assert!(agent.session.compaction.is_none());
    assert_eq!(
        serde_json::to_vec(&agent.session.messages[..raw_message_count_before]).unwrap(),
        raw_before
    );
}

#[test]
fn stale_projection_blocks_request_without_raw_fallback_or_context_mutation() {
    let provider = Arc::new(ProjectedRequestProvider::new(1_000));
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let mut session = Session::create(None, None);
    session.append_stored_message(context_test_text("stale-user", Role::User, "original"));
    session.append_stored_message(context_test_text(
        "stale-assistant",
        Role::Assistant,
        "answer",
    ));
    session.context_view = applied_context_state(vec![range_summary_operation(
        &session.messages,
        0,
        1,
        "stale summary",
    )]);
    session.messages[0].content = vec![ContentBlock::Text {
        text: "historically edited after draft".to_string(),
        cache_control: None,
    }];
    let mut agent = Agent::new_with_session(provider_dyn, Registry::empty(), session, None);
    let raw_before = serde_json::to_vec(&agent.session.messages).unwrap();

    let error = agent
        .messages_for_provider()
        .expect_err("stale target must block projected request");

    assert!(error.to_string().contains("provider request was not sent"));
    assert!(provider.requests().is_empty());
    assert_eq!(provider.summary_request_count(), 0);
    assert!(agent.session.compaction.is_none());
    assert_eq!(
        serde_json::to_vec(&agent.session.messages).unwrap(),
        raw_before
    );
}

#[test]
fn projected_append_preserves_cache_prefix_and_does_not_invalidate_continuation() {
    let _guard = crate::storage::lock_test_env();
    let previous = std::env::var_os("JCODE_TRACK_CLIENT_CACHE");
    crate::env::set_var("JCODE_TRACK_CLIENT_CACHE", "1");
    let provider = Arc::new(ProjectedRequestProvider::new(10_000));
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let mut agent = Agent::new(provider_dyn, Registry::empty());
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "first append baseline".to_string(),
            cache_control: None,
        }],
    );
    let first = agent.messages_for_provider().expect("first projection");
    agent.record_client_cache_request(&first);

    agent.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "second append".to_string(),
            cache_control: None,
        }],
    );
    let second = agent.messages_for_provider().expect("append projection");
    agent.record_client_cache_request(&second);

    assert_eq!(agent.cache_tracker.turn_count(), 2);
    assert_eq!(agent.cache_tracker.previous_message_count(), second.len());
    assert!(!agent.cache_tracker.had_violation());
    assert_eq!(provider.invalidation_count(), 0);
    match previous {
        Some(value) => crate::env::set_var("JCODE_TRACK_CLIENT_CACHE", value),
        None => crate::env::remove_var("JCODE_TRACK_CLIENT_CACHE"),
    }
}

#[tokio::test]
async fn historical_context_change_resets_provider_runtime_exactly_once() {
    let provider = Arc::new(ProjectedRequestProvider::new(10_000));
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let mut agent = Agent::new(provider_dyn, Registry::empty());
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "historical one".to_string(),
            cache_control: None,
        }],
    );
    agent.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "historical two".to_string(),
            cache_control: None,
        }],
    );
    let before = agent.messages_for_provider().expect("baseline projection");
    agent.cache_tracker.record_request(&before);
    agent.locked_tools = Some(Vec::new());
    agent.mcp_late_register_resolved = true;
    agent.provider_session_id = Some("agent-provider-session".to_string());
    agent.session.provider_session_id = Some("stored-provider-session".to_string());
    agent.update_context_usage_from_stream(9_000, None, None);
    let start = agent.session.messages.len() - 2;
    agent.session.context_view = applied_context_state(vec![range_summary_operation(
        &agent.session.messages,
        start,
        start + 1,
        "historical reset summary",
    )]);
    crate::cache_invalidation::clear_for_tests();
    let invalidation_started = Instant::now();

    agent
        .after_provider_context_changed("context transaction", "context revision 1 applied", true)
        .expect("changed projection remains valid");

    assert_eq!(provider.invalidation_count(), 1);
    assert_eq!(
        provider.invalidation_reasons(),
        vec!["context revision 1 applied".to_string()]
    );
    assert!(agent.provider_session_id.is_none());
    assert!(agent.session.provider_session_id.is_none());
    assert!(agent.locked_tools.is_none());
    assert!(!agent.mcp_late_register_resolved);
    assert_eq!(agent.cache_tracker.turn_count(), 0);
    let projected = agent.messages_for_provider().expect("changed projection");
    let stats = agent_context_budget_stats(&agent).await;
    assert_eq!(stats.observed_input_tokens, None);
    assert_eq!(stats.message_count, projected.len());
    assert_eq!(
        stats.estimated_message_tokens,
        estimated_message_tokens(&projected)
    );
    let documented = crate::cache_invalidation::most_recent_since(invalidation_started)
        .expect("intentional invalidation documented");
    assert_eq!(documented.source, "context transaction");
    assert_eq!(documented.detail, "context revision 1 applied");
}

fn drain_server_events(
    receiver: &mut tokio_mpsc::UnboundedReceiver<ServerEvent>,
) -> Vec<ServerEvent> {
    let mut events = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        events.push(event);
    }
    events
}

#[tokio::test]
async fn phase10_correlated_preflight_blocks_before_provider_call_and_rolls_back_pending_turn() {
    let provider = Arc::new(ScriptedSizeProvider::stream(128, Vec::new()));
    let mut agent = Agent::new(provider.clone(), Registry::empty());
    let raw_before = serde_json::to_vec(&agent.session.messages).unwrap();
    let (event_tx, mut event_rx) = tokio_mpsc::unbounded_channel();
    let prompt = "oversized Unicode prompt 🦀\nwith pasted code";

    let error = agent
        .run_once_streaming_mpsc_correlated(
            501,
            prompt,
            vec![("image/png".to_string(), "pending-image-data".to_string())],
            None,
            event_tx,
        )
        .await
        .expect_err("preflight must block");

    assert!(error.to_string().contains("Request not sent"));
    assert_eq!(provider.request_count(), 0);
    assert_eq!(
        serde_json::to_vec(&agent.session.messages).unwrap(),
        raw_before
    );
    let events = drain_server_events(&mut event_rx);
    assert!(events.iter().any(|event| matches!(
        event,
        ServerEvent::ContextPressureUpdated { id: 501, report, .. }
            if report.pressure == crate::protocol::ContextPressureLevel::Blocked
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ServerEvent::ContextActionRequired {
            id: 501,
            reason: crate::protocol::ContextActionRequiredReason::PreflightLimit,
            pending_input: Some(metadata),
            automatic_retry: false,
            ..
        } if metadata.matches(501, prompt, 1)
    )));
}

#[tokio::test]
async fn phase10_provider_payload_rejection_preserves_historical_images_and_never_retries() {
    let provider = Arc::new(ScriptedSizeProvider::open_error(
        1_000_000,
        "HTTP 413 payload too large",
    ));
    let mut agent = Agent::new(provider.clone(), Registry::empty());
    let historical_image = "historical-image-bytes".to_string();
    agent.add_message(
        Role::User,
        vec![
            ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: historical_image.clone(),
            },
            ContentBlock::Text {
                text: "historical image".to_string(),
                cache_control: None,
            },
        ],
    );
    let baseline_provider_messages = agent.session.messages_for_provider();
    agent
        .cache_tracker
        .record_request(&baseline_provider_messages);
    let cache_turns_before = agent.cache_tracker.turn_count();
    agent.mcp_late_register_resolved = true;
    agent.tool_output_scan_index = agent.session.messages.len();
    let raw_before = serde_json::to_vec(&agent.session.messages).unwrap();
    let (event_tx, mut event_rx) = tokio_mpsc::unbounded_channel();

    let error = agent
        .run_once_streaming_mpsc_correlated(
            502,
            "pending payload",
            vec![("image/png".to_string(), "pending-image-bytes".to_string())],
            None,
            event_tx,
        )
        .await
        .expect_err("provider must reject payload");

    assert!(error.to_string().contains("images were preserved"));
    assert_eq!(provider.request_count(), 1);
    assert_eq!(agent.cache_tracker.turn_count(), cache_turns_before);
    assert!(agent.locked_tools.is_none());
    assert!(agent.mcp_late_register_resolved);
    assert_eq!(agent.tool_output_scan_index, agent.session.messages.len());
    assert_eq!(
        serde_json::to_vec(&agent.session.messages).unwrap(),
        raw_before
    );
    assert!(agent.session.messages.iter().any(|message| {
        message.content.iter().any(
            |block| matches!(block, ContentBlock::Image { data, .. } if data == &historical_image),
        )
    }));
    let events = drain_server_events(&mut event_rx);
    assert!(events.iter().any(|event| matches!(
        event,
        ServerEvent::ContextActionRequired {
            id: 502,
            reason: crate::protocol::ContextActionRequiredReason::PayloadTooLarge,
            pending_input: Some(metadata),
            payload: Some(payload),
            automatic_retry: false,
            ..
        } if metadata.matches(502, "pending payload", 1)
            && payload.image_count == 2
            && payload.estimated_base64_bytes
                >= historical_image.len() + "pending-image-bytes".len()
    )));
}

#[tokio::test]
async fn phase10_post_output_context_rejection_preserves_partial_authoritative_output() {
    let provider = Arc::new(ScriptedSizeProvider::stream(
        1_000_000,
        vec![
            ScriptedSizeEvent::Event(StreamEvent::TextDelta("partial answer".to_string())),
            ScriptedSizeEvent::Event(StreamEvent::Error {
                message: "maximum context length exceeded".to_string(),
                retry_after_secs: None,
            }),
        ],
    ));
    let mut agent = Agent::new(provider.clone(), Registry::empty());
    let raw_len_before = agent.session.messages.len();
    let (event_tx, mut event_rx) = tokio_mpsc::unbounded_channel();

    agent
        .run_once_streaming_mpsc_correlated(503, "keep this turn", Vec::new(), None, event_tx)
        .await
        .expect_err("provider must reject continuation");

    assert_eq!(provider.request_count(), 1);
    assert!(agent.session.messages.len() >= raw_len_before + 2);
    assert!(agent.session.messages.iter().any(|message| {
        message.role == Role::Assistant
            && message.content.iter().any(
                |block| matches!(block, ContentBlock::Text { text, .. } if text == "partial answer"),
            )
    }));
    let events = drain_server_events(&mut event_rx);
    assert!(events.iter().any(|event| matches!(
        event,
        ServerEvent::ContextActionRequired {
            id: 503,
            reason: crate::protocol::ContextActionRequiredReason::ProviderContextLimit,
            pending_input: None,
            preflight: Some(report),
            automatic_retry: false,
            ..
        } if report.pressure == crate::protocol::ContextPressureLevel::Blocked
            && report.required_reduction_tokens >= 1
    )));
}

#[test]
fn phase10_partial_output_persistence_failure_is_reported_without_prompt_rollback() {
    let provider = Arc::new(ScriptedSizeProvider::stream(1_000_000, Vec::new()));
    let mut agent = Agent::new(provider, Registry::empty());
    agent.begin_pending_turn(
        Some(504),
        "authoritative prompt",
        0,
        8,
        0,
        crate::agent::PendingTurnOptions::default(),
    );
    agent.mark_provider_output_started();
    agent
        .active_turn_context
        .as_mut()
        .expect("active turn")
        .partial_output_checkpointed = true;
    agent
        .active_turn_context
        .as_mut()
        .expect("active turn")
        .partial_output_persistence_error = Some("injected persistence failure".to_string());
    let (event_tx, mut event_rx) = tokio_mpsc::unbounded_channel();

    let error = agent
        .handle_provider_size_rejection(
            "maximum context length exceeded",
            crate::protocol::ContextPayloadPressure {
                image_count: 0,
                estimated_base64_bytes: 0,
            },
            Some(&event_tx),
        )
        .expect("size rejection classified")
        .expect("actionable terminal error");

    assert!(error.to_string().contains("persistence failed"));
    assert!(agent.active_turn_context.is_some());
    let events = drain_server_events(&mut event_rx);
    assert!(events.iter().any(|event| matches!(
        event,
        ServerEvent::ContextActionRequired {
            id: 504,
            pending_input: None,
            details,
            automatic_retry: false,
            ..
        } if details.iter().any(|detail|
            detail == crate::protocol::CONTEXT_PARTIAL_OUTPUT_NOT_DURABLE)
    )));
}

#[test]
fn phase10_incomplete_provider_output_retains_turn_without_claiming_replayable_history() {
    let provider = Arc::new(ScriptedSizeProvider::stream(1_000_000, Vec::new()));
    let mut agent = Agent::new(provider, Registry::empty());
    agent.begin_pending_turn(
        Some(505),
        "authoritative prompt",
        0,
        8,
        0,
        crate::agent::PendingTurnOptions::default(),
    );
    agent.mark_provider_output_started();
    let (event_tx, mut event_rx) = tokio_mpsc::unbounded_channel();

    agent
        .handle_provider_size_rejection(
            "maximum context length exceeded",
            crate::protocol::ContextPayloadPressure {
                image_count: 0,
                estimated_base64_bytes: 0,
            },
            Some(&event_tx),
        )
        .expect("size rejection classified")
        .expect("actionable terminal error");

    assert!(agent.active_turn_context.is_some());
    let events = drain_server_events(&mut event_rx);
    assert!(events.iter().any(|event| matches!(
        event,
        ServerEvent::ContextActionRequired {
            id: 505,
            pending_input: None,
            details,
            automatic_retry: false,
            ..
        } if details.iter().any(|detail|
            detail == crate::protocol::CONTEXT_PARTIAL_OUTPUT_NOT_REPLAYABLE)
    )));
}

#[tokio::test]
async fn phase11_hidden_provider_limit_uses_one_authorized_transaction_and_one_retry() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let provider = HiddenLimitEmergencyProvider::default();
    let provider_handle = provider.clone();
    let provider: Arc<dyn Provider> = Arc::new(provider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "call-old".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({"command": "old"}),
            thought_signature: None,
        }],
    );
    agent.add_message(
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: "call-old".to_string(),
            content: "eligible older tool output ".repeat(600),
            is_error: Some(false),
        }],
    );
    agent.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "call-active".to_string(),
            name: "read".to_string(),
            input: serde_json::json!({"path": "active"}),
            thought_signature: None,
        }],
    );
    agent.add_message(
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: "call-active".to_string(),
            content: "protected active result".to_string(),
            is_error: Some(false),
        }],
    );
    let historical_reasoning = "historical replay reasoning ".repeat(600);
    agent.add_message(
        Role::Assistant,
        vec![
            ContentBlock::Reasoning {
                text: historical_reasoning.clone(),
            },
            ContentBlock::Text {
                text: "historical visible answer".to_string(),
                cache_control: None,
            },
        ],
    );
    let authorization = StoredUnattendedContextAuthorization {
        policy: StoredContextEmergencyPolicy::Authorized {
            protected_recent_assistant_turns: 0,
            target_headroom_percent: 1,
            allow_reasoning_suppression: true,
            allow_tool_distillation: true,
            allow_oldest_range_summary: false,
            authorization_source: "scheduled-item-policy".to_string(),
        },
        authorization_source: "scheduled_item:sched-hidden-limit".to_string(),
        scheduled_item_id: Some("sched-hidden-limit".to_string()),
    };

    let output = agent
        .run_once_capture_with_display_role_and_unattended(
            "continue scheduled work",
            Some(crate::session::StoredDisplayRole::System),
            Some(authorization),
        )
        .await
        .expect("authorized hidden-limit recovery succeeds");
    assert!(output.contains("recovered after one emergency transaction"));
    assert_eq!(provider_handle.calls.load(Ordering::SeqCst), 2);
    assert_eq!(provider_handle.invalidations.load(Ordering::SeqCst), 1);
    assert_eq!(agent.context_view_state().revision, 1);
    assert_eq!(agent.context_view_state().transactions.len(), 1);
    let transaction = &agent.context_view_state().transactions[0];
    assert_eq!(transaction.operations.len(), 1);
    assert!(matches!(
        transaction.operations[0],
        StoredContextOperation::ReasoningSuppression(_)
    ));
    let audit = transaction
        .emergency_audit
        .as_ref()
        .expect("audit retained");
    assert_eq!(
        audit.trigger_kind,
        jcode_session_types::StoredContextEmergencyTriggerKind::ProviderContextLimit
    );
    assert!(
        audit
            .provider_error
            .as_deref()
            .is_some_and(|error| error.contains("maximum context length exceeded"))
    );
    assert_eq!(
        audit.retry_outcome,
        jcode_session_types::StoredContextEmergencyRetryOutcome::Succeeded
    );
    assert!(agent.messages().iter().any(|message| {
        message.content.iter().any(|block| {
            matches!(block, ContentBlock::Reasoning { text } if text == &historical_reasoning)
        })
    }));

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[tokio::test]
async fn phase11_blocked_preflight_recovers_before_the_first_provider_call() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let provider = HiddenLimitEmergencyProvider {
        context_window: 50_000,
        fail_first_call: false,
        ..HiddenLimitEmergencyProvider::default()
    };
    let provider_handle = provider.clone();
    let provider: Arc<dyn Provider> = Arc::new(provider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::Assistant,
        vec![
            ContentBlock::Reasoning {
                text: "historical replay reasoning ".repeat(10_000),
            },
            ContentBlock::Text {
                text: "retain visible historical answer".to_string(),
                cache_control: None,
            },
        ],
    );
    let authorization = StoredUnattendedContextAuthorization {
        policy: StoredContextEmergencyPolicy::Authorized {
            protected_recent_assistant_turns: 0,
            target_headroom_percent: 1,
            allow_reasoning_suppression: true,
            allow_tool_distillation: false,
            allow_oldest_range_summary: false,
            authorization_source: "scheduled-item-policy".to_string(),
        },
        authorization_source: "scheduled_item:sched-preflight".to_string(),
        scheduled_item_id: Some("sched-preflight".to_string()),
    };

    let output = agent
        .run_once_capture_with_display_role_and_unattended(
            "continue scheduled work",
            Some(crate::session::StoredDisplayRole::System),
            Some(authorization),
        )
        .await
        .expect("authorized preflight recovery succeeds");
    assert!(output.contains("recovered after one emergency transaction"));
    assert_eq!(provider_handle.calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider_handle.invalidations.load(Ordering::SeqCst), 1);
    assert_eq!(agent.context_view_state().transactions.len(), 1);
    let audit = agent.context_view_state().transactions[0]
        .emergency_audit
        .as_ref()
        .expect("audit retained");
    assert_eq!(
        audit.trigger_kind,
        jcode_session_types::StoredContextEmergencyTriggerKind::PreflightLimit
    );
    assert_eq!(
        audit.retry_outcome,
        jcode_session_types::StoredContextEmergencyRetryOutcome::Succeeded
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[tokio::test]
async fn phase11_interactive_submit_blocks_even_when_session_policy_is_authorized() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let provider = HiddenLimitEmergencyProvider::default();
    let provider_handle = provider.clone();
    let provider: Arc<dyn Provider> = Arc::new(provider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.session.context_view.emergency_policy = StoredContextEmergencyPolicy::Authorized {
        protected_recent_assistant_turns: 0,
        target_headroom_percent: 1,
        allow_reasoning_suppression: true,
        allow_tool_distillation: false,
        allow_oldest_range_summary: false,
        authorization_source: "session-policy-must-not-authorize-interactive".to_string(),
    };
    agent.add_message(
        Role::Assistant,
        vec![ContentBlock::Reasoning {
            text: "historical replay reasoning ".repeat(600),
        }],
    );

    let error = agent
        .run_once_capture("ordinary interactive request")
        .await
        .expect_err("interactive request must retain Phase 10 blocking");
    assert!(!format!("{error:#}").is_empty());
    assert_eq!(provider_handle.calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider_handle.invalidations.load(Ordering::SeqCst), 0);
    assert_eq!(agent.context_view_state().revision, 0);
    assert!(agent.context_view_state().transactions.is_empty());

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[tokio::test]
async fn phase11_failed_retry_never_creates_a_second_emergency_transaction() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let provider = HiddenLimitEmergencyProvider {
        reject_retry: true,
        ..HiddenLimitEmergencyProvider::default()
    };
    let provider_handle = provider.clone();
    let provider: Arc<dyn Provider> = Arc::new(provider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::Assistant,
        vec![ContentBlock::Reasoning {
            text: "historical replay reasoning ".repeat(600),
        }],
    );
    let authorization = StoredUnattendedContextAuthorization {
        policy: StoredContextEmergencyPolicy::Authorized {
            protected_recent_assistant_turns: 0,
            target_headroom_percent: 1,
            allow_reasoning_suppression: true,
            allow_tool_distillation: false,
            allow_oldest_range_summary: false,
            authorization_source: "scheduled-item-policy".to_string(),
        },
        authorization_source: "scheduled_item:sched-retry-fails".to_string(),
        scheduled_item_id: Some("sched-retry-fails".to_string()),
    };

    let error = agent
        .run_once_capture_with_display_role_and_unattended(
            "continue scheduled work",
            Some(crate::session::StoredDisplayRole::System),
            Some(authorization),
        )
        .await
        .expect_err("the single retry remains rejected");
    assert!(!format!("{error:#}").is_empty());
    assert_eq!(provider_handle.calls.load(Ordering::SeqCst), 2);
    assert_eq!(provider_handle.invalidations.load(Ordering::SeqCst), 1);
    assert_eq!(agent.context_view_state().revision, 1);
    assert_eq!(agent.context_view_state().transactions.len(), 1);
    assert_eq!(
        agent.context_view_state().transactions[0]
            .emergency_audit
            .as_ref()
            .expect("audit retained")
            .retry_outcome,
        jcode_session_types::StoredContextEmergencyRetryOutcome::ProviderRejected
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[tokio::test]
async fn phase11_retry_that_fails_after_output_is_audited_as_failed_not_succeeded() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let provider = HiddenLimitEmergencyProvider {
        fail_retry_after_output: true,
        ..HiddenLimitEmergencyProvider::default()
    };
    let provider_handle = provider.clone();
    let provider: Arc<dyn Provider> = Arc::new(provider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::Assistant,
        vec![ContentBlock::Reasoning {
            text: "historical replay reasoning ".repeat(600),
        }],
    );
    let authorization = StoredUnattendedContextAuthorization {
        policy: StoredContextEmergencyPolicy::Authorized {
            protected_recent_assistant_turns: 0,
            target_headroom_percent: 1,
            allow_reasoning_suppression: true,
            allow_tool_distillation: false,
            allow_oldest_range_summary: false,
            authorization_source: "scheduled-item-policy".to_string(),
        },
        authorization_source: "scheduled_item:sched-output-fails".to_string(),
        scheduled_item_id: Some("sched-output-fails".to_string()),
    };

    let error = agent
        .run_once_capture_with_display_role_and_unattended(
            "continue scheduled work",
            Some(crate::session::StoredDisplayRole::System),
            Some(authorization),
        )
        .await
        .expect_err("retried stream fails after output starts");
    assert!(!format!("{error:#}").is_empty());
    assert_eq!(provider_handle.calls.load(Ordering::SeqCst), 2);
    assert_eq!(agent.context_view_state().transactions.len(), 1);
    assert!(matches!(
        agent.context_view_state().transactions[0]
            .emergency_audit
            .as_ref()
            .expect("audit retained")
            .retry_outcome,
        jcode_session_types::StoredContextEmergencyRetryOutcome::Failed { ref detail }
            if detail.contains("retry stream failed after output")
    ));

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[tokio::test]
async fn context_budget_tracks_agent_create_append_usage_and_model_switch_without_transforming_context()
 {
    let provider: Arc<dyn Provider> = Arc::new(SwitchableBudgetProvider::new("small"));
    let registry = Registry::empty();
    let mut agent = Agent::new(provider, registry);

    let initial_provider_messages = agent.messages_for_provider().expect("initial projection");
    let initial_stats = agent_context_budget_stats(&agent).await;
    assert_eq!(initial_stats.token_budget, 10_000);
    assert_eq!(initial_stats.message_count, initial_provider_messages.len());
    assert_eq!(
        initial_stats.estimated_message_tokens,
        estimated_message_tokens(&initial_provider_messages)
    );

    let messages_before_accounting = serde_json::to_vec(&agent.session.messages).unwrap();
    let context_before_accounting = serde_json::to_vec(&agent.session.context_view).unwrap();
    agent.update_context_usage_from_stream(9_000, None, None);
    assert_eq!(
        agent_context_budget_stats(&agent)
            .await
            .observed_input_tokens,
        Some(9_000)
    );

    agent.set_model("large").expect("switch model");
    let switched_stats = agent_context_budget_stats(&agent).await;
    assert_eq!(switched_stats.token_budget, 50_000);
    assert_eq!(switched_stats.observed_input_tokens, None);
    assert_eq!(
        serde_json::to_vec(&agent.session.messages).unwrap(),
        messages_before_accounting
    );
    assert_eq!(
        serde_json::to_vec(&agent.session.context_view).unwrap(),
        context_before_accounting
    );

    let stats_before_append = switched_stats;
    let context_before_append = serde_json::to_vec(&agent.session.context_view).unwrap();
    let old_messages = agent.session.messages.clone();
    let appended_content = vec![ContentBlock::Text {
        text: "accounted append".to_string(),
        cache_control: None,
    }];
    agent.add_message(Role::User, appended_content.clone());
    let stats_after_append = agent_context_budget_stats(&agent).await;
    assert_eq!(
        stats_after_append.message_count,
        stats_before_append.message_count.saturating_add(1)
    );
    assert_eq!(
        stats_after_append.estimated_message_tokens,
        stats_before_append
            .estimated_message_tokens
            .saturating_add(estimated_content_tokens(appended_content))
    );
    assert_eq!(
        serde_json::to_vec(&agent.session.messages[..old_messages.len()]).unwrap(),
        serde_json::to_vec(&old_messages).unwrap()
    );
    assert_eq!(
        serde_json::to_vec(&agent.session.context_view).unwrap(),
        context_before_append
    );

    agent
        .set_model("small")
        .expect("switch back to small model");
    agent.update_context_usage_from_stream(9_500, None, None);
    agent
        .set_route_selection(&crate::provider::RouteSelection {
            model: "large".to_string(),
            runtime_key: crate::provider::RuntimeKey::Current,
            api_method: "current".to_string(),
            provider_label: "current".to_string(),
            detail: String::new(),
        })
        .expect("switch route");
    let route_switched_stats = agent_context_budget_stats(&agent).await;
    assert_eq!(route_switched_stats.token_budget, 50_000);
    assert_eq!(route_switched_stats.observed_input_tokens, None);
    assert_eq!(
        serde_json::to_vec(&agent.session.context_view).unwrap(),
        context_before_append
    );

    let provider_messages = agent.messages_for_provider().expect("reseed projection");
    agent.reseed_context_budget_from_messages(&provider_messages, "test reseed");
    assert_eq!(
        serde_json::to_vec(&agent.session.context_view).unwrap(),
        context_before_append
    );
}

#[tokio::test]
async fn unsupported_provider_switch_preserves_raw_context_state_and_continuation() {
    let provider = Arc::new(NoValidationSwitchProvider::new("supported-before"));
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let mut agent = Agent::new(provider_dyn, Registry::empty());
    let messages = vec![context_test_message(
        "switch-source",
        Role::User,
        vec![ContentBlock::Text {
            text: "authoritative source".to_string(),
            cache_control: None,
        }],
    )];
    agent.session.replace_messages(messages.clone());
    agent.session.context_view = applied_context_state(vec![range_summary_operation(
        &messages,
        0,
        0,
        "Stored provider-neutral summary",
    )]);
    agent.session.model = Some("supported-before".to_string());
    agent.session.provider_session_id = Some("persisted-continuation".to_string());
    agent.provider_session_id = Some("runtime-continuation".to_string());
    let messages_before = serde_json::to_vec(&agent.session.messages).unwrap();
    let context_before = serde_json::to_vec(&agent.session.context_view).unwrap();

    let error = agent
        .set_model("unsupported-after")
        .expect_err("candidate without a production validation adapter must be rejected");

    assert!(
        error
            .to_string()
            .contains("no production request-builder validation adapter")
    );
    assert_eq!(provider.model(), "supported-before");
    assert_eq!(agent.session.model.as_deref(), Some("supported-before"));
    assert_eq!(
        agent.provider_session_id.as_deref(),
        Some("runtime-continuation")
    );
    assert_eq!(
        agent.session.provider_session_id.as_deref(),
        Some("persisted-continuation")
    );
    assert_eq!(provider.invalidations.load(Ordering::SeqCst), 0);
    assert_eq!(
        serde_json::to_vec(&agent.session.messages).unwrap(),
        messages_before
    );
    assert_eq!(
        serde_json::to_vec(&agent.session.context_view).unwrap(),
        context_before
    );
}

#[tokio::test]
async fn legacy_summary_attach_migrates_to_projection_and_invalidates_once() {
    let provider = Arc::new(ProjectedRequestProvider::new(10_000));
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let mut session = Session::create(None, None);
    for index in 0..4 {
        session.add_message(
            if index % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            },
            vec![ContentBlock::Text {
                text: format!("raw-{index}-{}", "x".repeat(8_000)),
                cache_control: None,
            }],
        );
    }
    let raw_before = serde_json::to_vec(&session.messages).unwrap();
    session.compaction = Some(crate::session::StoredCompactionState {
        summary_text: "concise legacy summary".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 2,
        original_turn_count: 4,
        compacted_count: 2,
    });
    session.provider_session_id = Some("stale-migrated-session".to_string());

    let registry = Registry::empty();
    let mut agent = Agent::new_with_session(provider_dyn, registry, session, None);
    let effective_messages = agent.messages_for_provider().expect("migrated projection");
    let raw_messages = agent.session.raw_messages_for_provider_uncached();
    let stats = agent_context_budget_stats(&agent).await;
    let effective_tokens = estimated_message_tokens(&effective_messages);
    assert_eq!(stats.message_count, effective_messages.len());
    assert_eq!(stats.estimated_message_tokens, effective_tokens);
    assert!(effective_tokens < estimated_message_tokens(&raw_messages));
    assert!(agent.session.compaction.is_none());
    assert!(agent.session.provider_session_id.is_none());
    assert_eq!(agent.session.context_view.active_transaction_count(), 1);
    assert_eq!(provider.invalidation_count(), 1);
    assert_eq!(
        provider.invalidation_reasons(),
        vec!["legacy context migration activated while attaching session".to_string()]
    );
    assert_eq!(
        serde_json::to_vec(&agent.session.messages[..4]).unwrap(),
        raw_before
    );
}

#[tokio::test]
async fn context_budget_rewind_undo_and_repair_reseed_exactly_and_clear_observation() {
    let provider = Arc::new(ProjectedRequestProvider::new(10_000));
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let registry = Registry::empty();
    let mut agent = Agent::new(provider_dyn, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "first visible message".to_string(),
            cache_control: None,
        }],
    );
    agent.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "second visible message".to_string(),
            cache_control: None,
        }],
    );
    let before_rewind = agent_context_budget_stats(&agent).await;
    agent.update_context_usage_from_stream(9_500, None, None);
    agent.provider_session_id = Some("rewind-agent-session".to_string());
    agent.session.provider_session_id = Some("rewind-stored-session".to_string());

    assert_eq!(agent.rewind_to_message(1).expect("rewind"), 1);
    assert_eq!(provider.invalidation_count(), 1);
    assert!(agent.provider_session_id.is_none());
    assert!(agent.session.provider_session_id.is_none());
    let rewound_messages = agent.messages_for_provider().expect("rewound projection");
    let rewound_stats = agent_context_budget_stats(&agent).await;
    assert_eq!(rewound_stats.observed_input_tokens, None);
    assert_eq!(rewound_stats.message_count, rewound_messages.len());
    assert_eq!(
        rewound_stats.estimated_message_tokens,
        estimated_message_tokens(&rewound_messages)
    );

    agent.provider_session_id = Some("undo-agent-session".to_string());
    agent.session.provider_session_id = Some("undo-stored-session".to_string());
    assert_eq!(agent.undo_rewind().expect("undo rewind"), 1);
    assert_eq!(provider.invalidation_count(), 2);
    assert!(agent.provider_session_id.is_none());
    assert!(agent.session.provider_session_id.is_none());
    let restored_stats = agent_context_budget_stats(&agent).await;
    assert_eq!(restored_stats.observed_input_tokens, None);
    assert_eq!(restored_stats.message_count, before_rewind.message_count);
    assert_eq!(
        restored_stats.estimated_message_tokens,
        before_rewind.estimated_message_tokens
    );

    let context_before_repair = serde_json::to_vec(&agent.session.context_view).unwrap();
    agent.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "missing-result-call".to_string(),
            name: "read".to_string(),
            input: serde_json::json!({"file_path": "src/lib.rs"}),
            thought_signature: None,
        }],
    );
    agent.update_context_usage_from_stream(9_900, None, None);
    agent.provider_session_id = Some("repair-agent-session".to_string());
    agent.session.provider_session_id = Some("repair-stored-session".to_string());
    assert_eq!(agent.repair_missing_tool_outputs(), 1);

    assert_eq!(provider.invalidation_count(), 3);
    assert!(agent.provider_session_id.is_none());
    assert!(agent.session.provider_session_id.is_none());
    let repaired_messages = agent.messages_for_provider().expect("repaired projection");
    let repaired_stats = agent_context_budget_stats(&agent).await;
    assert_eq!(repaired_stats.observed_input_tokens, None);
    assert_eq!(repaired_stats.message_count, repaired_messages.len());
    assert_eq!(
        repaired_stats.estimated_message_tokens,
        estimated_message_tokens(&repaired_messages)
    );
    assert_eq!(
        serde_json::to_vec(&agent.session.context_view).unwrap(),
        context_before_repair
    );
}

#[tokio::test]
async fn rewind_invalidates_only_removed_transaction_sources_and_undo_restores_exact_state() {
    let provider = Arc::new(ProjectedRequestProvider::new(10_000));
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let mut agent = Agent::new(provider_dyn, Registry::empty());
    let messages = vec![
        context_test_message(
            "retained-assistant",
            Role::Assistant,
            vec![
                ContentBlock::Reasoning {
                    text: "retained reasoning".to_string(),
                },
                ContentBlock::Text {
                    text: "retained text".to_string(),
                    cache_control: None,
                },
            ],
        ),
        context_test_message(
            "removed-assistant",
            Role::Assistant,
            vec![
                ContentBlock::Reasoning {
                    text: "removed reasoning".to_string(),
                },
                ContentBlock::Text {
                    text: "removed text".to_string(),
                    cache_control: None,
                },
            ],
        ),
    ];
    let context_state = StoredContextViewState {
        revision: 2,
        transactions: vec![
            context_test_transaction(
                "retained-transaction",
                0,
                1,
                vec![reasoning_suppression_operation(&messages, 0, 0)],
            ),
            context_test_transaction(
                "removed-transaction",
                1,
                2,
                vec![reasoning_suppression_operation(&messages, 1, 0)],
            ),
        ],
        ..StoredContextViewState::default()
    };
    agent.session.replace_messages(messages);
    agent.session.context_view = context_state;
    let messages_before = serde_json::to_vec(&agent.session.messages).unwrap();
    let context_before = serde_json::to_vec(&agent.session.context_view).unwrap();

    assert_eq!(agent.rewind_to_message(1).expect("rewind"), 1);
    assert_eq!(agent.session.context_view.revision, 3);
    assert!(agent.session.context_view.transactions[0].is_active());
    assert_eq!(
        agent.session.context_view.transactions[1]
            .latest_status()
            .expect("status")
            .kind,
        StoredContextTransactionStatusKind::InvalidatedByTranscriptEdit
    );
    assert_eq!(provider.invalidation_count(), 1);
    agent
        .messages_for_provider()
        .expect("valid rewound projection");

    assert_eq!(agent.undo_rewind().expect("undo"), 1);
    assert_eq!(provider.invalidation_count(), 2);
    assert_eq!(
        serde_json::to_vec(&agent.session.messages).unwrap(),
        messages_before
    );
    assert_eq!(
        serde_json::to_vec(&agent.session.context_view).unwrap(),
        context_before
    );
    agent
        .messages_for_provider()
        .expect("valid restored projection");
}

#[tokio::test]
async fn missing_tool_repair_invalidates_summary_that_is_no_longer_structurally_closed() {
    let provider = Arc::new(ProjectedRequestProvider::new(10_000));
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let mut agent = Agent::new(provider_dyn, Registry::empty());
    let messages = vec![context_test_message(
        "historical-tool-call",
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "missing-historical-result".to_string(),
            name: "read".to_string(),
            input: serde_json::json!({"file_path": "src/lib.rs"}),
            thought_signature: None,
        }],
    )];
    agent.session.replace_messages(messages.clone());
    agent.session.context_view = applied_context_state(vec![range_summary_operation(
        &messages,
        0,
        0,
        "Historical tool work before its repair",
    )]);

    assert_eq!(agent.repair_missing_tool_outputs(), 1);

    assert_eq!(agent.session.messages.len(), 2);
    assert_eq!(agent.session.context_view.revision, 2);
    assert_eq!(
        agent.session.context_view.transactions[0]
            .latest_status()
            .expect("status")
            .kind,
        StoredContextTransactionStatusKind::InvalidatedByTranscriptEdit
    );
    assert_eq!(provider.invalidation_count(), 1);
    let projected = agent
        .messages_for_provider()
        .expect("valid repaired projection");
    assert_eq!(projected.len(), 2);
}

// ── InterruptSignal tests ────────────────────────────────────────────────

#[tokio::test]
async fn interrupt_signal_fire_before_notified_does_not_hang() {
    // Regression test: fire() called BEFORE notified().await must not hang.
    // The old code called notify_waiters() which drops the notification if
    // nobody is waiting yet. The flag is still set so the fast path catches it,
    // but only if the future is created before the flag check.
    let sig = InterruptSignal::new();
    sig.fire(); // fire before anyone is waiting
    tokio::time::timeout(std::time::Duration::from_millis(100), sig.notified())
        .await
        .expect("notified() hung when signal was already set before call");
}

#[tokio::test]
async fn interrupt_signal_fire_concurrent_with_notified() {
    // Regression test for the race window: fire() is called concurrently while
    // notified() is being set up. The fix (create future before flag check) ensures
    // the notify_waiters() in fire() wakes the registered future.
    let sig = Arc::new(InterruptSignal::new());
    let sig2 = Arc::clone(&sig);

    // Spawn a task that fires after a tiny delay, giving the main task time to
    // enter notified() but before it reaches notified().await.
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        sig2.fire();
    });

    tokio::time::timeout(std::time::Duration::from_millis(500), sig.notified())
        .await
        .expect("notified() hung during concurrent fire()");
}

#[tokio::test]
async fn interrupt_signal_is_set_false_initially() {
    let sig = InterruptSignal::new();
    assert!(!sig.is_set());
}

#[tokio::test]
async fn interrupt_signal_is_set_true_after_fire() {
    let sig = InterruptSignal::new();
    sig.fire();
    assert!(sig.is_set());
}

#[tokio::test]
async fn interrupt_signal_reset_clears_flag() {
    let sig = InterruptSignal::new();
    sig.fire();
    assert!(sig.is_set());
    sig.reset();
    assert!(!sig.is_set());
}

#[tokio::test]
async fn interrupt_signal_notified_completes_after_fire() {
    let sig = Arc::new(InterruptSignal::new());
    let sig2 = Arc::clone(&sig);

    let handle = tokio::spawn(async move {
        sig2.notified().await;
    });

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    sig.fire();

    tokio::time::timeout(std::time::Duration::from_millis(200), handle)
        .await
        .expect("notified() task timed out after fire()")
        .expect("task panicked");
}

#[tokio::test]
async fn new_agent_registers_active_pid_and_clear_swaps_it() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(ImmediateEmptyProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    let first_session_id = agent.session_id().to_string();
    assert!(
        crate::session::active_session_ids().contains(&first_session_id),
        "fresh agent session should be tracked as active"
    );

    agent.clear();

    let second_session_id = agent.session_id().to_string();
    let active = crate::session::active_session_ids();
    assert_ne!(first_session_id, second_session_id);
    assert!(
        active.contains(&second_session_id),
        "replacement session should be tracked as active"
    );
    assert!(
        !active.contains(&first_session_id),
        "cleared session should no longer be tracked as active"
    );
}

#[tokio::test]
async fn gmail_is_exposed_by_default_and_can_be_explicitly_disabled() {
    let _guard = crate::storage::lock_test_env();
    let prev_home = std::env::var_os("JCODE_HOME");
    let prev_tools = std::env::var_os("JCODE_TOOLS");
    let prev_disabled_tools = std::env::var_os("JCODE_DISABLED_TOOLS");
    let prev_tool_profile = std::env::var_os("JCODE_TOOL_PROFILE");
    let prev_disable_base_tools = std::env::var_os("JCODE_DISABLE_BASE_TOOLS");
    let temp_home = tempfile::TempDir::new().expect("temp home");

    crate::env::set_var("JCODE_HOME", temp_home.path());
    crate::env::remove_var("JCODE_TOOLS");
    crate::env::remove_var("JCODE_DISABLED_TOOLS");
    crate::env::remove_var("JCODE_TOOL_PROFILE");
    crate::env::remove_var("JCODE_DISABLE_BASE_TOOLS");
    crate::config::Config::invalidate_cache();

    let provider: Arc<dyn Provider> = Arc::new(ImmediateEmptyProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let definitions = agent.tool_definitions().await;
    let tool_names = agent.tool_names().await;
    let tool_name = "gmail";

    assert!(
        tool_names.iter().any(|name| name == "jcode_docs"),
        "jcode_docs must be model-visible in regular sessions"
    );
    assert!(
        !tool_names.iter().any(|name| name == "selfdev"),
        "selfdev must not be model-visible in regular sessions"
    );

    assert!(
        definitions
            .iter()
            .any(|definition| definition.name == tool_name),
        "{tool_name} must be sent in model-visible tool definitions by default"
    );
    assert!(
        tool_names.iter().any(|name| name == tool_name),
        "{tool_name} must be listed as model-visible by default"
    );
    agent
        .validate_tool_allowed(tool_name)
        .expect("gmail must be executable by default");

    crate::env::set_var("JCODE_DISABLED_TOOLS", tool_name);
    crate::config::Config::invalidate_cache();

    let provider: Arc<dyn Provider> = Arc::new(ImmediateEmptyProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let definitions = agent.tool_definitions().await;
    let tool_names = agent.tool_names().await;

    assert!(
        !definitions
            .iter()
            .any(|definition| definition.name == tool_name),
        "explicitly disabled {tool_name} must not be sent in model-visible tool definitions"
    );
    assert!(
        !tool_names.iter().any(|name| name == tool_name),
        "explicitly disabled {tool_name} must not be listed as model-visible"
    );
    let err = agent
        .validate_tool_allowed(tool_name)
        .expect_err("explicitly disabled gmail must not be executable");
    assert!(err.to_string().contains("disabled"));

    if let Some(previous) = prev_home {
        crate::env::set_var("JCODE_HOME", previous);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    if let Some(previous) = prev_tools {
        crate::env::set_var("JCODE_TOOLS", previous);
    } else {
        crate::env::remove_var("JCODE_TOOLS");
    }
    if let Some(previous) = prev_disabled_tools {
        crate::env::set_var("JCODE_DISABLED_TOOLS", previous);
    } else {
        crate::env::remove_var("JCODE_DISABLED_TOOLS");
    }
    if let Some(previous) = prev_tool_profile {
        crate::env::set_var("JCODE_TOOL_PROFILE", previous);
    } else {
        crate::env::remove_var("JCODE_TOOL_PROFILE");
    }
    if let Some(previous) = prev_disable_base_tools {
        crate::env::set_var("JCODE_DISABLE_BASE_TOOLS", previous);
    } else {
        crate::env::remove_var("JCODE_DISABLE_BASE_TOOLS");
    }
    crate::config::Config::invalidate_cache();
}

fn seed_transient_session_state(agent: &mut Agent) {
    agent.push_alert("pending alert".to_string());
    agent.queue_soft_interrupt(
        "queued interrupt".to_string(),
        Vec::new(),
        true,
        SoftInterruptSource::User,
    );
    agent.background_tool_signal.fire();
    agent.request_graceful_shutdown();
    agent.tool_call_ids.insert("tool_call_old".to_string());
    agent.tool_result_ids.insert("tool_result_old".to_string());
    agent.tool_output_scan_index = 7;
    agent.last_upstream_provider = Some("upstream_old".to_string());
    agent.last_connection_type = Some("websocket".to_string());
    agent.current_turn_system_reminder = Some("reminder".to_string());
    agent.last_usage = TokenUsage {
        input_tokens: 11,
        output_tokens: 17,
        cache_read_input_tokens: Some(3),
        cache_creation_input_tokens: Some(5),
    };
    agent.locked_tools = Some(vec![ToolDefinition {
        name: "test_tool".to_string(),
        description: "test tool".to_string(),
        input_schema: serde_json::json!({"type": "object"}),
    }]);
}

#[tokio::test]
async fn clear_resets_runtime_interrupt_and_queue_state() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(ImmediateEmptyProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    seed_transient_session_state(&mut agent);
    assert_eq!(agent.soft_interrupt_count(), 1);
    assert!(agent.background_tool_signal().is_set());
    assert!(agent.graceful_shutdown_signal().is_set());

    agent.clear();

    assert_eq!(agent.soft_interrupt_count(), 0);
    assert!(!agent.background_tool_signal().is_set());
    assert!(!agent.graceful_shutdown_signal().is_set());
    assert_eq!(agent.pending_alert_count(), 0);
    assert!(agent.tool_call_ids.is_empty());
    assert!(agent.tool_result_ids.is_empty());
    assert_eq!(agent.tool_output_scan_index, 0);
    assert!(agent.last_upstream_provider.is_none());
    assert!(agent.last_connection_type.is_none());
    assert!(agent.current_turn_system_reminder.is_none());
    assert_eq!(agent.last_usage.input_tokens, 0);
    assert_eq!(agent.last_usage.output_tokens, 0);
    assert!(agent.locked_tools.is_none());
}

#[tokio::test]
async fn restore_session_resets_runtime_interrupt_and_queue_state() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(ImmediateEmptyProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    let mut restored_session = crate::session::Session::create_with_id(
        "session_restore_resets_runtime_state".to_string(),
        None,
        None,
    );
    restored_session.save().expect("save restored session");

    seed_transient_session_state(&mut agent);
    assert_eq!(agent.soft_interrupt_count(), 1);
    assert!(agent.background_tool_signal().is_set());
    assert!(agent.graceful_shutdown_signal().is_set());

    let status = agent
        .restore_session(&restored_session.id)
        .expect("restore session should succeed");

    assert_eq!(status, crate::session::SessionStatus::Active);
    assert_eq!(agent.session_id(), restored_session.id);
    assert_eq!(agent.soft_interrupt_count(), 0);
    assert!(!agent.background_tool_signal().is_set());
    assert!(!agent.graceful_shutdown_signal().is_set());
    assert_eq!(agent.pending_alert_count(), 0);
    assert!(agent.tool_call_ids.is_empty());
    assert!(agent.tool_result_ids.is_empty());
    assert_eq!(agent.tool_output_scan_index, 0);
    assert!(agent.last_upstream_provider.is_none());
    assert!(agent.last_connection_type.is_none());
    assert!(agent.current_turn_system_reminder.is_none());
    assert_eq!(agent.last_usage.input_tokens, 0);
    assert_eq!(agent.last_usage.output_tokens, 0);
    assert!(agent.locked_tools.is_none());
}

#[tokio::test]
async fn restore_session_rejects_unsupported_projection_before_live_state_changes() {
    let _guard = crate::storage::lock_test_env();
    let provider = Arc::new(NoValidationSwitchProvider::new("current-model"));
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let registry = Registry::new(provider_dyn.clone()).await;
    let mut agent = Agent::new(provider_dyn, registry);
    let original_session_id = agent.session_id().to_string();

    let messages = vec![context_test_message(
        "restore-source",
        Role::User,
        vec![ContentBlock::Text {
            text: "restore source".to_string(),
            cache_control: None,
        }],
    )];
    let mut restored = crate::session::Session::create_with_id(
        "unsupported_projected_restore".to_string(),
        None,
        None,
    );
    restored.replace_messages(messages.clone());
    restored.context_view = applied_context_state(vec![range_summary_operation(
        &messages,
        0,
        0,
        "stored summary",
    )]);
    restored.model = Some("candidate-model".to_string());
    restored.save().expect("save restore fixture");

    let error = agent
        .restore_session(&restored.id)
        .expect_err("restore must reject a provider without production validation");

    assert!(
        error
            .to_string()
            .contains("no production request-builder validation adapter")
    );
    assert_eq!(agent.session_id(), original_session_id);
    assert_eq!(provider.model(), "current-model");
    assert_eq!(provider.invalidations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn explicit_provider_pin_is_persisted_and_reapplied_on_restore() {
    let _guard = crate::storage::lock_test_env();
    let provider = Arc::new(ExplicitPinProvider::new("z-ai/glm-5.2"));
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let registry = Registry::new(provider_dyn.clone()).await;
    let mut agent = Agent::new(provider_dyn, registry);

    agent
        .set_model("z-ai/glm-5.2@Novita")
        .expect("set explicitly pinned model");
    assert_eq!(agent.provider_model(), "z-ai/glm-5.2@Novita");
    let persisted = crate::session::Session::load(agent.session_id()).expect("load saved session");
    assert_eq!(persisted.model.as_deref(), Some("z-ai/glm-5.2@Novita"));

    let restored_provider = Arc::new(ExplicitPinProvider::new("other/model"));
    let restored_provider_dyn: Arc<dyn Provider> = restored_provider.clone();
    let restored_registry = Registry::new(restored_provider_dyn.clone()).await;
    let restored_agent =
        Agent::new_with_session(restored_provider_dyn, restored_registry, persisted, None);

    assert_eq!(
        restored_provider
            .set_model_requests
            .lock()
            .unwrap()
            .as_slice(),
        ["openrouter:z-ai/glm-5.2@Novita"]
    );
    assert_eq!(restored_agent.provider_model(), "z-ai/glm-5.2@Novita");
}

#[tokio::test]
async fn restore_session_rehydrates_injected_memory_ids() {
    let _guard = crate::storage::lock_test_env();
    crate::memory::clear_all_pending_memory();

    let provider: Arc<dyn Provider> = Arc::new(ImmediateEmptyProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    let mut restored_session = crate::session::Session::create_with_id(
        "session_restore_memory_dedup".to_string(),
        None,
        None,
    );
    restored_session.record_memory_injection(
        "🧠 auto-recalled 1 memory".to_string(),
        "persisted memory".to_string(),
        1,
        5,
        vec!["memory-persisted".to_string()],
    );
    restored_session.save().expect("save restored session");

    crate::memory::mark_memories_injected(&restored_session.id, &["memory-stale".to_string()]);

    agent
        .restore_session(&restored_session.id)
        .expect("restore session should succeed");

    assert!(crate::memory::is_memory_injected(
        &restored_session.id,
        "memory-persisted"
    ));
    assert!(
        !crate::memory::is_memory_injected(&restored_session.id, "memory-stale"),
        "restore should replace stale in-memory dedup state with persisted session data"
    );

    crate::memory::clear_all_pending_memory();
}

#[tokio::test]
async fn build_memory_prompt_nonblocking_defers_pending_memory_during_tool_loop() {
    let _guard = crate::storage::lock_test_env();
    crate::memory::clear_all_pending_memory();

    let provider: Arc<dyn Provider> = Arc::new(ImmediateEmptyProvider);
    let registry = Registry::new(provider.clone()).await;
    let agent = Agent::new(provider, registry);
    let session_id = agent.session.id.clone();

    crate::memory::set_pending_memory_with_ids(
        &session_id,
        "remember this later".to_string(),
        1,
        vec!["memory-deferred".to_string()],
    );

    let tool_loop_messages = vec![
        Message::user("hello"),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_1".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({}),
                thought_signature: None,
            }],
            timestamp: Some(chrono::Utc::now()),
            tool_duration_ms: None,
        },
        Message::tool_result("call_1", "ok", false),
    ];

    let pending = agent.build_memory_prompt_nonblocking(&tool_loop_messages, None);
    assert!(pending.is_none(), "memory should not inject mid tool loop");
    assert!(crate::memory::has_pending_memory(&session_id));

    let next_turn_messages = vec![Message::user("follow up")];
    let pending = agent.build_memory_prompt_nonblocking(&next_turn_messages, None);
    assert!(
        pending.is_some(),
        "memory should inject on the next real user turn"
    );
    assert!(!crate::memory::has_pending_memory(&session_id));

    crate::memory::clear_all_pending_memory();
}

#[tokio::test]
async fn memory_injection_message_defaults_to_ephemeral_history() {
    let _guard = crate::storage::lock_test_env();
    let previous = std::env::var_os("JCODE_PERSIST_MEMORY_INJECTIONS");
    crate::env::set_var("JCODE_PERSIST_MEMORY_INJECTIONS", "false");
    crate::config::invalidate_config_cache();

    let provider: Arc<dyn Provider> = Arc::new(ImmediateEmptyProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let before = agent.session.messages.len();
    let memory = crate::memory::PendingMemory {
        prompt: "# Memory\n\n## Facts\n1. Use ephemeral mode".to_string(),
        display_prompt: None,
        computed_at: Instant::now(),
        count: 1,
        memory_ids: vec!["mem-ephemeral".to_string()],
    };

    let (message, persisted) = agent.prepare_memory_injection_message(&memory);

    assert!(!persisted);
    assert_eq!(agent.session.messages.len(), before);
    assert!(matches!(message.role, Role::User));
    assert!(message_text(&message).contains("Use ephemeral mode"));

    match previous {
        Some(value) => crate::env::set_var("JCODE_PERSIST_MEMORY_INJECTIONS", value),
        None => crate::env::remove_var("JCODE_PERSIST_MEMORY_INJECTIONS"),
    }
    crate::config::invalidate_config_cache();
}

#[tokio::test]
async fn memory_injection_message_can_persist_to_history() {
    let _guard = crate::storage::lock_test_env();
    let previous = std::env::var_os("JCODE_PERSIST_MEMORY_INJECTIONS");
    crate::env::set_var("JCODE_PERSIST_MEMORY_INJECTIONS", "true");
    crate::config::invalidate_config_cache();

    let provider: Arc<dyn Provider> = Arc::new(ImmediateEmptyProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let before = agent.session.messages.len();
    let memory = crate::memory::PendingMemory {
        prompt: "# Memory\n\n## Facts\n1. Persist for cache".to_string(),
        display_prompt: None,
        computed_at: Instant::now(),
        count: 1,
        memory_ids: vec!["mem-persisted".to_string()],
    };

    let (message, persisted) = agent.prepare_memory_injection_message(&memory);

    assert!(persisted);
    assert_eq!(agent.session.messages.len(), before + 1);
    assert_eq!(
        content_text(&agent.session.messages.last().unwrap().content),
        message_text(&message)
    );
    assert!(
        content_text(&agent.session.messages.last().unwrap().content).contains("Persist for cache")
    );

    match previous {
        Some(value) => crate::env::set_var("JCODE_PERSIST_MEMORY_INJECTIONS", value),
        None => crate::env::remove_var("JCODE_PERSIST_MEMORY_INJECTIONS"),
    }
    crate::config::invalidate_config_cache();
}

#[tokio::test]
async fn mark_closed_persists_soft_interrupts_for_restore_after_reload() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let provider: Arc<dyn Provider> = Arc::new(ImmediateEmptyProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider.clone(), registry.clone());
    let session_id = agent.session_id().to_string();
    agent.session.save().expect("save active session");
    agent.queue_soft_interrupt(
        "resume me after reload".to_string(),
        Vec::new(),
        true,
        SoftInterruptSource::System,
    );

    agent.mark_closed();

    let mut restored = Agent::new(provider, registry);
    restored
        .restore_session(&session_id)
        .expect("restore session with persisted interrupts");

    assert_eq!(restored.soft_interrupt_count(), 1);
    assert!(restored.has_urgent_interrupt());
    assert!(
        crate::soft_interrupt_store::load(&session_id)
            .expect("store should be readable after restore")
            .is_empty()
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[tokio::test]
async fn soft_interrupt_injection_preserves_exact_authorization_scope_and_persists_remainder() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let provider: Arc<dyn Provider> = Arc::new(ImmediateEmptyProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.session.save().expect("save session");
    agent.begin_pending_turn(
        None,
        "interactive prompt",
        0,
        4,
        0,
        crate::agent::PendingTurnOptions::default(),
    );
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "interactive prompt".to_string(),
            cache_control: None,
        }],
    );
    let authorization = |id: &str| StoredUnattendedContextAuthorization {
        policy: StoredContextEmergencyPolicy::Authorized {
            protected_recent_assistant_turns: 5,
            target_headroom_percent: 10,
            allow_reasoning_suppression: true,
            allow_tool_distillation: true,
            allow_oldest_range_summary: true,
            authorization_source: format!("schedule_tool_session:{id}"),
        },
        authorization_source: format!("scheduled_item:{id}"),
        scheduled_item_id: Some(id.to_string()),
    };
    let first = authorization("sched-a");
    let first_policy = first.policy.clone();
    let second = authorization("sched-b");
    {
        let queue = agent.soft_interrupt_queue();
        let mut queue = queue.lock().unwrap();
        for content in ["a-1", "a-2"] {
            queue.push(SoftInterruptMessage {
                content: content.to_string(),
                images: Vec::new(),
                urgent: false,
                source: SoftInterruptSource::System,
                unattended_context: Some(first.clone()),
            });
        }
        queue.push(SoftInterruptMessage {
            content: "b-1".to_string(),
            images: Vec::new(),
            urgent: false,
            source: SoftInterruptSource::System,
            unattended_context: Some(second.clone()),
        });
    }

    let injected = agent.inject_soft_interrupts();
    assert_eq!(injected.len(), 1);
    assert!(injected[0].content.contains("a-1"));
    assert!(injected[0].content.contains("a-2"));
    assert!(!injected[0].content.contains("b-1"));
    assert_eq!(
        agent
            .active_turn_context
            .as_ref()
            .and_then(|context| context.unattended_context.clone()),
        Some(first)
    );
    assert_eq!(
        agent
            .active_turn_context
            .as_ref()
            .map(|context| context.transcript_len_before_pending),
        Some(0),
        "the original interactive prompt and scheduled prompt share one protected boundary"
    );
    assert_eq!(agent.soft_interrupt_count(), 1);
    let persisted = crate::soft_interrupt_store::load(agent.session_id()).unwrap();
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0].unattended_context, Some(second.clone()));

    let audit_transaction_id = "context-emergency-scope-a".to_string();
    agent.session.context_view.revision = 1;
    agent
        .session
        .context_view
        .transactions
        .push(StoredContextTransaction {
            id: audit_transaction_id.clone(),
            base_revision: 0,
            created_at: chrono::Utc::now(),
            authorization: StoredContextAuthorization::UnattendedEmergency {
                authorization_source: "scheduled_item:sched-a".to_string(),
                trigger: Some("preflight_limit".to_string()),
                scheduled_item_id: Some("sched-a".to_string()),
            },
            operations: Vec::new(),
            status_events: vec![StoredContextStatusEvent {
                revision: 1,
                timestamp: chrono::Utc::now(),
                kind: StoredContextTransactionStatusKind::Applied,
                reason: None,
            }],
            application: None,
            economics: None,
            curator_usage: Vec::new(),
            emergency_audit: Some(StoredContextEmergencyAudit {
                authorization_source: "scheduled_item:sched-a".to_string(),
                scheduled_item_id: Some("sched-a".to_string()),
                policy: first_policy,
                trigger_kind: StoredContextEmergencyTriggerKind::PreflightLimit,
                provider_error: None,
                context_window: 100_000,
                safe_input_budget: 95_000,
                projected_input_tokens: 96_000,
                required_reduction_to_fit_tokens: 1_000,
                required_reduction_to_target_tokens: 10_500,
                achieved_reduction_tokens: 11_000,
                protected_recent_assistant_turns: 5,
                protected_message_count: 3,
                operation_order: vec![StoredContextEmergencyOperationKind::ReasoningSuppression],
                retry_outcome: StoredContextEmergencyRetryOutcome::Pending,
            }),
        });
    agent
        .active_turn_context
        .as_mut()
        .expect("active turn")
        .emergency_transaction_id = Some(audit_transaction_id);

    let injected = agent.inject_soft_interrupts();
    assert_eq!(injected.len(), 1);
    assert!(injected[0].content.contains("b-1"));
    assert_eq!(
        agent
            .active_turn_context
            .as_ref()
            .and_then(|context| context.unattended_context.clone()),
        Some(second)
    );
    assert_eq!(
        agent.session.context_view.transactions[0]
            .emergency_audit
            .as_ref()
            .expect("audit")
            .retry_outcome,
        StoredContextEmergencyRetryOutcome::Succeeded,
        "scope A must finalize before scope B replaces turn-local authorization"
    );
    assert_eq!(agent.soft_interrupt_count(), 0);
    assert!(
        crate::soft_interrupt_store::load(agent.session_id())
            .unwrap()
            .is_empty()
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[tokio::test]
async fn env_snapshot_detail_is_minimal_for_empty_sessions_and_full_after_history() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(ImmediateEmptyProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    assert_eq!(agent.env_snapshot_detail(), EnvSnapshotDetail::Minimal);
    let minimal = agent.build_env_snapshot("create", agent.env_snapshot_detail());
    assert!(minimal.jcode_git_hash.is_none());
    assert!(minimal.jcode_git_dirty.is_none());
    assert!(minimal.working_git.is_none());

    agent
        .session
        .append_stored_message(crate::session::StoredMessage {
            id: "msg_env_snapshot_detail".to_string(),
            role: crate::message::Role::User,
            content: vec![ContentBlock::Text {
                text: "hello".to_string(),
                cache_control: None,
            }],
            display_role: None,
            timestamp: None,
            tool_duration_ms: None,
            token_usage: None,
        });

    assert_eq!(agent.env_snapshot_detail(), EnvSnapshotDetail::Full);
}

/// A trivial tool used to simulate an MCP tool registering on the registry
/// after the agent has already locked its tool snapshot.
struct FakeMcpTool {
    name: String,
}

#[async_trait]
impl crate::tool::Tool for FakeMcpTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "fake mcp tool"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn execute(
        &self,
        _input: serde_json::Value,
        _ctx: crate::tool::ToolContext,
    ) -> anyhow::Result<ToolOutput> {
        Ok(ToolOutput::new("ok"))
    }
}

/// Reproduction for #206: MCP tools that register on the registry *after* the
/// first turn locks the tool snapshot never reach the provider, because
/// `tool_definitions()` returns the frozen `locked_tools` snapshot and the only
/// unlock path (`unlock_tools_if_needed`) fires solely when the LLM invokes the
/// `"mcp"` management tool — which it never does, since it cannot see the
/// `mcp__*` tools it would need to trigger that unlock.
#[tokio::test]
async fn mcp_tools_registered_after_lock_are_visible_to_agent() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(ImmediateEmptyProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    // First turn locks the snapshot (this is what happens before the async MCP
    // registration spawn completes).
    let before = agent.tool_definitions().await;
    let before_len = before.len();
    assert!(
        !before.iter().any(|t| t.name.starts_with("mcp__")),
        "precondition: no mcp tools before async registration completes"
    );

    // Simulate the spawned MCP registration task finishing: a new mcp__* tool
    // lands on the shared registry.
    agent
        .registry
        .register(
            "mcp__test__write_memory".to_string(),
            Arc::new(FakeMcpTool {
                name: "mcp__test__write_memory".to_string(),
            }) as Arc<dyn crate::tool::Tool>,
        )
        .await;

    // The next turn should now advertise the MCP tool to the provider.
    let after = agent.tool_definitions().await;
    assert!(
        after.iter().any(|t| t.name == "mcp__test__write_memory"),
        "regression #206: MCP tool registered after the first turn never reaches \
         the agent's tool surface (locked snapshot of {} tools is reused forever)",
        before_len
    );

    // Once MCP tools are present in the locked snapshot, subsequent turns must
    // return the *same* stable snapshot so provider prompt-cache hits stay warm
    // (the whole point of locked_tools). The #206 fix must not flap.
    let names =
        |defs: &[ToolDefinition]| -> Vec<String> { defs.iter().map(|t| t.name.clone()).collect() };
    let stable_a = agent.tool_definitions().await;
    let stable_b = agent.tool_definitions().await;
    assert_eq!(
        names(&stable_a),
        names(&stable_b),
        "tool snapshot must be stable across turns once MCP tools are present"
    );
    assert_eq!(
        names(&stable_a),
        names(&after),
        "snapshot must not change after MCP tools are already included"
    );
}

/// The intentional, MCP-driven prompt-cache miss must happen at most ONCE per
/// locked snapshot. After the first late-registered `mcp__*` tool is picked up
/// (the one accepted miss), a *second* MCP tool that registers even later must
/// NOT trigger another rebuild — otherwise a server that connects in waves would
/// thrash the provider prompt cache. Guards the `mcp_late_register_resolved`
/// one-shot flag (#206 follow-up).
#[tokio::test]
async fn mcp_late_registration_rebuild_happens_at_most_once() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(ImmediateEmptyProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    // First turn locks the snapshot with no MCP tools yet.
    let _ = agent.tool_definitions().await;

    // First MCP tool arrives -> one accepted rebuild exposes it.
    agent
        .registry
        .register(
            "mcp__test__first".to_string(),
            Arc::new(FakeMcpTool {
                name: "mcp__test__first".to_string(),
            }) as Arc<dyn crate::tool::Tool>,
        )
        .await;
    let after_first = agent.tool_definitions().await;
    assert!(
        after_first.iter().any(|t| t.name == "mcp__test__first"),
        "first late MCP tool must be picked up by the one accepted rebuild"
    );
    assert!(
        agent.mcp_late_register_resolved,
        "one-shot guard must latch after the accepted rebuild"
    );

    // A SECOND MCP tool registers even later (server connected in a second
    // wave). The one-shot guard means we do NOT rebuild again, so the snapshot
    // stays cache-stable and this tool is intentionally not surfaced until the
    // tool list is explicitly unlocked.
    agent
        .registry
        .register(
            "mcp__test__second".to_string(),
            Arc::new(FakeMcpTool {
                name: "mcp__test__second".to_string(),
            }) as Arc<dyn crate::tool::Tool>,
        )
        .await;
    let after_second = agent.tool_definitions().await;
    let names: Vec<String> = after_second.iter().map(|t| t.name.clone()).collect();
    assert!(
        names.iter().any(|n| n == "mcp__test__first"),
        "previously surfaced MCP tool must remain"
    );
    assert!(
        !names.iter().any(|n| n == "mcp__test__second"),
        "second-wave MCP tool must NOT trigger a second cache-busting rebuild"
    );

    // An explicit unlock (e.g. the `mcp` reload tool) re-arms the one-shot guard
    // and lets the next snapshot pick up everything currently registered.
    agent.unlock_tools();
    assert!(
        !agent.mcp_late_register_resolved,
        "explicit unlock must re-arm the one-shot guard"
    );
    let after_unlock = agent.tool_definitions().await;
    let unlocked_names: Vec<String> = after_unlock.iter().map(|t| t.name.clone()).collect();
    assert!(
        unlocked_names.iter().any(|n| n == "mcp__test__second"),
        "after explicit unlock, the second-wave MCP tool must finally surface"
    );
}

/// Without any newly-registered MCP tools, the locked snapshot must be returned
/// verbatim on every turn (no rebuild, no cache invalidation). Guards the #206
/// fix against re-snapshotting on turns where nothing changed.
#[tokio::test]
async fn tool_snapshot_is_stable_without_new_mcp_tools() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(ImmediateEmptyProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    let first = agent.tool_definitions().await;
    // Register a NON-mcp tool after locking — this should NOT trigger a rebuild,
    // because the cache-stability optimization only yields to MCP arrival.
    agent
        .registry
        .register(
            "not_an_mcp_tool".to_string(),
            Arc::new(FakeMcpTool {
                name: "not_an_mcp_tool".to_string(),
            }) as Arc<dyn crate::tool::Tool>,
        )
        .await;
    let second = agent.tool_definitions().await;
    let first_names: Vec<String> = first.iter().map(|t| t.name.clone()).collect();
    let second_names: Vec<String> = second.iter().map(|t| t.name.clone()).collect();
    assert_eq!(
        first_names, second_names,
        "non-MCP registry changes must not invalidate the locked tool snapshot"
    );
    assert!(
        !second_names.iter().any(|n| n == "not_an_mcp_tool"),
        "non-MCP tool registered after lock must not leak into the snapshot"
    );
}

#[test]
fn empty_post_tool_response_gets_more_than_one_retry() {
    // Regression guard for the Claude Opus 5 benchmark incident. A provider can
    // return an empty response immediately after tool results; that is a
    // transient hiccup, not a finished task. With only one retry allowed, a
    // single empty response (observed once in 43 turns) ended a 20-hour agent
    // run with the work half-done and the submission unoptimized.
    const {
        assert!(
            Agent::MAX_EMPTY_POST_TOOL_CONTINUATION_ATTEMPTS > 1,
            "a single retry lets one transient empty response end a long run"
        );
        // Bounded, so a genuinely finished agent still exits instead of looping.
        assert!(Agent::MAX_EMPTY_POST_TOOL_CONTINUATION_ATTEMPTS <= 10);
    }
}

#[test]
fn output_budget_truncation_requests_a_continuation() {
    // Regression guard for the Claude Opus 5 benchmark incident. A turn cut off
    // by the output budget reports stop_reason=max_tokens and can contain zero
    // tool calls, which otherwise looks exactly like a finished turn. The agent
    // must treat it as incomplete and continue rather than ending the run.
    assert!(Agent::should_continue_after_stop_reason("max_tokens"));
    assert!(Agent::should_continue_after_stop_reason("MAX_TOKENS"));
    assert!(Agent::should_continue_after_stop_reason(" max_tokens "));
    assert!(Agent::should_continue_after_stop_reason(
        "max_output_tokens"
    ));
    assert!(Agent::should_continue_after_stop_reason("length"));
    assert!(Agent::should_continue_after_stop_reason("truncated"));
    assert!(Agent::should_continue_after_stop_reason("incomplete"));

    // Normal completions must not trigger a continuation loop.
    assert!(!Agent::should_continue_after_stop_reason("end_turn"));
    assert!(!Agent::should_continue_after_stop_reason("tool_use"));
    assert!(!Agent::should_continue_after_stop_reason("stop"));
    // An absent reason is the pre-fix wire behaviour: it cannot be recovered
    // from, which is precisely why MessageEnd must forward the real reason.
    assert!(!Agent::should_continue_after_stop_reason(""));
}

#[test]
fn stranded_tool_use_stop_is_detected() {
    // Second half of the Opus 5 DeepSWE incident: the provider reported
    // stop_reason="tool_use" while the parsed tool-call list was empty, so the
    // turn loop had nothing to execute and broke out mid-task, discarding every
    // uncommitted edit. `tool_use` is a normal completion reason, so
    // `should_continue_after_stop_reason` must keep rejecting it; the stranded
    // case is only recoverable when it is paired with zero tool calls, which is
    // exactly what this predicate is for.
    assert!(Agent::is_stranded_tool_use_stop(Some("tool_use")));
    assert!(Agent::is_stranded_tool_use_stop(Some("TOOL_USE")));
    assert!(Agent::is_stranded_tool_use_stop(Some(" tool_use ")));

    assert!(!Agent::is_stranded_tool_use_stop(Some("end_turn")));
    assert!(!Agent::is_stranded_tool_use_stop(Some("max_tokens")));
    assert!(!Agent::is_stranded_tool_use_stop(Some("")));
    assert!(!Agent::is_stranded_tool_use_stop(None));
    // Must stay disjoint from the truncation path so a turn never takes both
    // continuation branches for one stop reason.
    assert!(!Agent::should_continue_after_stop_reason("tool_use"));
}

#[test]
fn guardrail_stop_reason_detection() {
    assert!(Agent::is_guardrail_stop_reason(Some("refusal")));
    assert!(Agent::is_guardrail_stop_reason(Some("REFUSAL")));
    assert!(Agent::is_guardrail_stop_reason(Some(" content_filter ")));
    assert!(Agent::is_guardrail_stop_reason(Some("safety")));
    assert!(Agent::is_guardrail_stop_reason(Some("model_guardrail")));
    assert!(Agent::is_guardrail_stop_reason(Some("policy_violation_x")));
    assert!(!Agent::is_guardrail_stop_reason(Some("end_turn")));
    assert!(!Agent::is_guardrail_stop_reason(Some("max_tokens")));
    assert!(!Agent::is_guardrail_stop_reason(Some("tool_use")));
    assert!(!Agent::is_guardrail_stop_reason(Some("stop")));
    assert!(!Agent::is_guardrail_stop_reason(None));
}

#[test]
fn fable_guardrail_reconsideration_is_narrow_and_bounded() {
    assert!(Agent::should_reconsider_fable_guardrail(
        "claude-fable-5",
        Some("refusal"),
        0,
        1,
    ));
    assert!(Agent::should_reconsider_fable_guardrail(
        "CLAUDE-FABLE-5-20260801",
        Some("content_filter"),
        0,
        1,
    ));
    assert!(Agent::should_reconsider_fable_guardrail(
        "claude-fable-5",
        Some("refusal"),
        1,
        3,
    ));
    assert!(Agent::should_reconsider_fable_guardrail(
        "claude-fable-5",
        Some("refusal"),
        2,
        3,
    ));
    assert!(!Agent::should_reconsider_fable_guardrail(
        "claude-fable-5",
        Some("refusal"),
        3,
        3,
    ));
    assert!(!Agent::should_reconsider_fable_guardrail(
        "claude-fable-5",
        Some("end_turn"),
        0,
        1,
    ));
    assert!(!Agent::should_reconsider_fable_guardrail(
        "claude-opus-5",
        Some("refusal"),
        0,
        1,
    ));
}

#[test]
fn fable_guardrail_prompt_suite_is_distinct_and_safety_preserving() {
    let prompts = Agent::FABLE_GUARDRAIL_RECONSIDERATION_PROMPTS;
    assert_eq!(prompts.len(), 3);
    assert_ne!(prompts[0], prompts[1]);
    assert_ne!(prompts[1], prompts[2]);
    assert!(prompts[0].contains("full context"));
    assert!(prompts[1].contains("safe portions"));
    assert!(prompts[2].contains("Do not weaken a refusal"));
}

#[test]
fn guardrail_notice_for_refusal_stop() {
    let notice = Agent::provider_guardrail_notice(Some("refusal"), true, true)
        .expect("refusal with empty text must produce a notice");
    assert!(
        notice.contains("refusal"),
        "notice should name the stop reason: {notice}"
    );
    assert!(notice.to_lowercase().contains("guardrail"));
    // Guardrail stop with visible text still surfaces (partial output then refusal).
    assert!(Agent::provider_guardrail_notice(Some("refusal"), false, false).is_some());
}

#[test]
fn guardrail_notice_for_silent_empty_turn() {
    // end_turn with zero visible output and reasoning-only content: surface it.
    let notice = Agent::provider_guardrail_notice(Some("end_turn"), true, true)
        .expect("empty visible output must produce a notice");
    assert!(notice.contains("internal reasoning"), "{notice}");
    assert!(notice.contains("end_turn"), "{notice}");
    // Unknown stop reason, empty output, no reasoning.
    let notice = Agent::provider_guardrail_notice(None, true, false)
        .expect("empty visible output must produce a notice");
    assert!(notice.contains("unknown"), "{notice}");
    assert!(!notice.contains("internal reasoning"), "{notice}");
}

#[test]
fn guardrail_notice_absent_for_normal_turns() {
    // Normal turn with visible text: no notice.
    assert!(Agent::provider_guardrail_notice(Some("end_turn"), false, false).is_none());
    assert!(Agent::provider_guardrail_notice(None, false, true).is_none());
}

#[test]
fn empty_turn_log_event_separates_guardrails_from_transient_empties() {
    assert_eq!(
        Agent::empty_turn_log_event(Some("refusal")),
        "PROVIDER_GUARDRAIL"
    );
    assert_eq!(
        Agent::empty_turn_log_event(Some("content_filter")),
        "PROVIDER_GUARDRAIL"
    );
    assert_eq!(
        Agent::empty_turn_log_event(Some("stop")),
        "PROVIDER_EMPTY_RESPONSE"
    );
    assert_eq!(Agent::empty_turn_log_event(None), "PROVIDER_EMPTY_RESPONSE");
}

#[test]
fn guardrail_notice_for_transient_empty_does_not_blame_content_filter() {
    let notice = Agent::provider_guardrail_notice(Some("stop"), true, false)
        .expect("empty visible output must produce a notice");
    assert!(
        !notice.contains("usually a provider-side guardrail"),
        "transient empty responses must not be blamed on a guardrail: {notice}"
    );
    assert!(notice.contains("empty response"), "{notice}");
}

#[tokio::test]
async fn empty_post_tool_response_is_retried_in_shared_helper() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(ImmediateEmptyProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    let mut attempts = 0u32;
    // Empty response right after tool results: inject continuation.
    let retried = agent
        .maybe_continue_empty_post_tool_response(true, true, Some("stop"), &mut attempts)
        .expect("helper must not error");
    assert!(retried);
    assert_eq!(attempts, 1);
    let recovery = agent
        .session
        .messages
        .last()
        .expect("recovery instruction must be persisted");
    assert_eq!(recovery.role, Role::User);
    assert!(
        recovery
            .content
            .iter()
            .find_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .is_some_and(|text| text.starts_with("<system-reminder>")),
        "synthetic recovery instruction must be hidden from the transcript"
    );

    // A guardrail refusal is deliberate and must not be retried.
    let retried = agent
        .maybe_continue_empty_post_tool_response(true, true, Some("refusal"), &mut attempts)
        .expect("helper must not error");
    assert!(!retried);

    // Visible output or no recent tool result: no retry.
    assert!(
        !agent
            .maybe_continue_empty_post_tool_response(false, true, Some("stop"), &mut attempts)
            .unwrap()
    );
    assert!(
        !agent
            .maybe_continue_empty_post_tool_response(true, false, Some("stop"), &mut attempts)
            .unwrap()
    );

    // Retry budget is bounded.
    attempts = Agent::MAX_EMPTY_POST_TOOL_CONTINUATION_ATTEMPTS;
    assert!(
        !agent
            .maybe_continue_empty_post_tool_response(true, true, Some("stop"), &mut attempts)
            .unwrap()
    );
}

include!("agent_tests/retention_readiness.rs");

/// Provider that reproduces the DeepSWE Opus 5 incident: the first response
/// ends with `stop_reason: "tool_use"` while carrying no tool-use block at all,
/// which is what happens when an unrecognized content block is dropped from the
/// stream. The second response is a normal completion, so a correct agent
/// recovers and this provider's queue is exhausted.
#[derive(Clone, Default)]
struct StrandedToolUseProvider {
    calls: Arc<std::sync::Mutex<usize>>,
}

#[async_trait]
impl Provider for StrandedToolUseProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let call = {
            let mut guard = self.calls.lock().unwrap();
            *guard += 1;
            *guard
        };
        let (tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(8);
        tokio::spawn(async move {
            if call == 1 {
                let _ = tx
                    .send(Ok(StreamEvent::TextDelta("working on it".to_string())))
                    .await;
                // No ToolUseStart: the tool block was lost, yet the provider
                // still reports that it stopped in order to call a tool.
                let _ = tx
                    .send(Ok(StreamEvent::MessageEnd {
                        stop_reason: Some("tool_use".to_string()),
                    }))
                    .await;
            } else {
                let _ = tx
                    .send(Ok(StreamEvent::TextDelta("all done".to_string())))
                    .await;
                let _ = tx
                    .send(Ok(StreamEvent::MessageEnd {
                        stop_reason: Some("end_turn".to_string()),
                    }))
                    .await;
            }
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "stranded-tool-use"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

/// End-to-end guard for the incident. Before the fix the agent took the
/// "no tool calls" branch and ended the turn on the very first response, so a
/// benchmark trial stopped mid-task and its uncommitted work was never
/// captured. The agent must instead ask the model to continue, which shows up
/// as a second provider call and a final turn that ends normally.
#[tokio::test]
async fn stranded_tool_use_stop_continues_instead_of_ending_the_turn() {
    let _guard = crate::storage::lock_test_env();
    let stranded = StrandedToolUseProvider::default();
    let calls = stranded.calls.clone();
    let provider: Arc<dyn Provider> = Arc::new(stranded);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    agent
        .run_once_streaming_mpsc("do the task", Vec::new(), None, tx)
        .await
        .expect("turn should complete");

    let mut text = String::new();
    while let Ok(event) = rx.try_recv() {
        if let ServerEvent::TextDelta { text: delta } = event {
            text.push_str(&delta);
        }
    }

    assert_eq!(
        *calls.lock().unwrap(),
        2,
        "a tool_use stop with no tool call must trigger exactly one continuation request"
    );
    assert!(
        text.contains("all done"),
        "the recovered turn must deliver the model's real completion, got {text:?}"
    );
}

#[derive(Clone, Default)]
struct FableGuardrailProvider {
    calls: Arc<std::sync::Mutex<usize>>,
    prompts_seen: Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait]
impl Provider for FableGuardrailProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let call = {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            *calls
        };
        if call > 1 {
            let prompt = messages
                .last()
                .map(message_text)
                .unwrap_or_default()
                .to_string();
            self.prompts_seen.lock().unwrap().push(prompt);
        }

        let (tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(4);
        tokio::spawn(async move {
            if call <= 3 {
                let _ = tx
                    .send(Ok(StreamEvent::MessageEnd {
                        stop_reason: Some("refusal".to_string()),
                    }))
                    .await;
            } else {
                let _ = tx
                    .send(Ok(StreamEvent::TextDelta(
                        "Reconsidered and completed safely".to_string(),
                    )))
                    .await;
                let _ = tx
                    .send(Ok(StreamEvent::MessageEnd {
                        stop_reason: Some("end_turn".to_string()),
                    }))
                    .await;
            }
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "anthropic"
    }

    fn model(&self) -> String {
        "claude-fable-5".to_string()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[tokio::test]
async fn fable_guardrail_reconsideration_recovers_the_streaming_turn() {
    let _guard = crate::storage::lock_test_env();
    let fable = FableGuardrailProvider::default();
    let calls = fable.calls.clone();
    let prompts_seen = fable.prompts_seen.clone();
    let provider: Arc<dyn Provider> = Arc::new(fable);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    agent
        .run_once_streaming_mpsc("do this ordinary coding task", Vec::new(), None, tx)
        .await
        .expect("turn should recover from the guardrail");

    let mut text = String::new();
    while let Ok(event) = rx.try_recv() {
        if let ServerEvent::TextDelta { text: delta } = event {
            text.push_str(&delta);
        }
    }

    assert_eq!(*calls.lock().unwrap(), 4);
    let prompts = prompts_seen.lock().unwrap();
    assert_eq!(prompts.len(), 3);
    assert!(prompts[0].contains("concrete harmful action"));
    assert!(prompts[1].contains("safe portions"));
    assert!(prompts[2].contains("final, independent policy check"));
    assert!(
        text.contains("Reconsidered and completed safely"),
        "{text:?}"
    );
}
