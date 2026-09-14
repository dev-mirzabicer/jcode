//! Native localhost provider + real Registry/roster/Agent/store integration.
use super::*;
use futures::FutureExt;
use std::collections::VecDeque;
use std::sync::Mutex;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Semaphore;

enum Reply {
    Text(&'static str),
    Tool(&'static str, Value),
    Hold(Arc<Semaphore>),
}
struct Http {
    url: String,
    script: Arc<Mutex<VecDeque<Reply>>>,
    requests: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Http {
    async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let script = Arc::new(Mutex::new(VecDeque::new()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let s = script.clone();
        let r = requests.clone();
        let task = tokio::spawn(async move {
            let mut handlers = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.unwrap(); let script=s.clone();let requests=r.clone();
                        handlers.spawn(async move {
                            let (reader, mut writer)=stream.into_split();let mut reader=BufReader::new(reader);
                            let mut length=0; let mut line=String::new();
                            loop {
                                line.clear();
                                if reader.read_line(&mut line).await.unwrap() == 0 { return; }
                                if line == "\r\n" { break; }
                                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                                    length = value.trim().parse().unwrap();
                                }
                            }
                            let mut bytes=vec![0;length];reader.read_exact(&mut bytes).await.unwrap();
                            let input:Value=serde_json::from_slice(&bytes).unwrap(); let ordinal={let mut requests=requests.lock().unwrap();requests.push(input);requests.len()};
                            let reply=script.lock().unwrap().pop_front().expect("unexpected provider request");
                            writer.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").await.unwrap();
                            let (delta, finish)=match reply {
                                Reply::Text(text)=>(json!({"role":"assistant","content":text}),"stop"),
                                Reply::Tool(name,args)=>(json!({"role":"assistant","tool_calls":[{"index":0,"id":format!("fixture-call-{ordinal}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]}),"tool_calls"),
                                Reply::Hold(gate)=>{
                                    let event=json!({"id":"fixture","object":"chat.completion.chunk","model":"fixture-model","choices":[{"index":0,"delta":{"role":"assistant","content":"PARTIAL"},"finish_reason":null}]});
                                    writer.write_all(format!("data: {event}\n\n").as_bytes()).await.unwrap();
                                    let permit=gate.acquire().await.unwrap();permit.forget();
                                    (json!({"content":" DONE"}),"stop")
                                }
                            };
                            for (delta,reason) in [(delta,Value::Null),(json!({}),json!(finish))] {
                                let event=json!({"id":"fixture","object":"chat.completion.chunk","created":0,"model":"fixture-model","choices":[{"index":0,"delta":delta,"finish_reason":reason}]});
                                if writer.write_all(format!("data: {event}\n\n").as_bytes()).await.is_err(){return;}
                            }
                            let _=writer.write_all(b"data: [DONE]\n\n").await;
                        });
                    },
                    _ = handlers.join_next(), if !handlers.is_empty()=>{},
                }
            }
        });
        Self {
            url,
            script,
            requests,
            task,
        }
    }
    fn push(&self, reply: Reply) {
        self.script.lock().unwrap().push_back(reply);
    }
}
impl Drop for Http {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Template;
#[async_trait::async_trait]
impl Provider for Template {
    fn name(&self) -> &str {
        "parent-fixture"
    }
    fn model(&self) -> String {
        "unchanged-parent-model".into()
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
    fn model_routes(&self) -> Vec<crate::provider::ModelRoute> {
        vec![crate::provider::ModelRoute {
            model: "fixture-model".into(),
            api_method: "openai-compatible:fixture".into(),
            provider: "fixture".into(),
            available: true,
            detail: String::new(),
            cheapness: None,
        }]
    }
    async fn complete(
        &self,
        _: &[crate::message::Message],
        _: &[crate::message::ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        anyhow::bail!("Parent provider must never be invoked or mutated by delegation")
    }
}

struct Fixture {
    home: crate::auth::test_sandbox::AuthTestSandbox,
    http: Http,
    parent: Session,
    registry: Registry,
    ids: Mutex<Vec<String>>,
    host: Arc<Host>,
}
impl Fixture {
    async fn new() -> Self {
        let home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
        let http = Http::new().await;
        std::fs::write(home.root().join("config.toml"),format!("[features]\nmemory=false\nswarm=false\n[sponsors]\nenabled=false\n[delegation]\nmax_running_children=1\n[providers.fixture]\ntype='openai-compatible'\nbase_url='{}'\nauth='none'\nrequires_api_key=false\ndefault_model='fixture-model'\n",http.url)).unwrap();
        crate::config::invalidate_config_cache();
        crate::provider::external::register_openrouter_factory(|spec| {
            use crate::provider::external::OpenRouterRuntimeSpec as S;
            use jcode_provider_openrouter_runtime::OpenRouterProvider as P;
            let provider: Arc<dyn Provider> = match spec {
                S::JcodeSubscription => Arc::new(P::new_subscription_execution()?),
                S::NamedProfileForExecution { name, config } => {
                    Arc::new(P::new_named_execution(&name, &config)?)
                }
                S::Default => Arc::new(P::new()?),
                S::OpenRouterApiKey => Arc::new(P::new_openrouter_api_key_runtime()?),
                S::CompatibleProfile(profile) => {
                    Arc::new(P::new_openai_compatible_profile_runtime(profile)?)
                }
                S::NamedProfile { name, config } => {
                    Arc::new(P::new_named_openai_compatible(&name, &config)?)
                }
            };
            Ok(provider)
        });
        let repositories = InstructionRepositoryService::new();
        SystemPromptComposer::from_repository_service(repositories.clone())
            .ensure_global_store()
            .unwrap();
        std::fs::write(
            home.root().join("instructions/model-roster.toml"),
            "[aliases.fixture]\ndescription='synthetic alias'\nmodels=['fixture:fixture-model']\n",
        )
        .unwrap();
        std::fs::write(home.root().join("instructions/agents/fixture.md"),"---\nid: fixture\nkind: agent\nname: Fixture\ndescription: synthetic\navailability: both\n---\nFROZEN PROFILE").unwrap();
        std::fs::write(
            home.root().join("instructions/system/subagent.md"),
            "---\nid: subagent\nkind: system\n---\nFROZEN CHILD SYSTEM",
        )
        .unwrap();
        std::fs::write(home.root().join("instructions/notifications/task-preset.special.md"),"---\nid: task-preset.special\nkind: notification\nname: special\ndescription: synthetic\n---\nFIRST SPECIAL").unwrap();
        let project = home.root().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let mut parent = Session::create(None, None);
        parent.working_dir = Some(project.to_str().unwrap().into());
        parent.add_human_message(vec![crate::message::ContentBlock::Text {
            text: "PARENT-PRIVATE-TEXT".into(),
            cache_control: None,
        }]);
        parent.save().unwrap();
        let host = Arc::new(
            Host::new(
                Arc::new(Template),
                Arc::new(crate::mcp::SharedMcpPool::new(
                    crate::mcp::McpConfig::default(),
                )),
                repositories,
            )
            .unwrap(),
        );
        let registry = Registry::empty();
        for tool in crate::tool::subagent::DelegationTool::hosted(host.clone()) {
            registry.register(tool.name().into(), Arc::new(tool)).await;
        }
        Self {
            home,
            http,
            parent,
            registry,
            ids: Default::default(),
            host,
        }
    }
    fn ctx(&self, id: &str) -> ToolContext {
        ToolContext {
            session_id: self.parent.id.clone(),
            message_id: format!("message-{id}"),
            tool_call_id: id.into(),
            working_dir: self.parent.working_dir.as_ref().map(PathBuf::from),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::AgentTurn,
            invocation: Default::default(),
        }
    }
    fn child_id(&self, id: &str) -> String {
        format!(
            "session_child_{}",
            crate::execution::invocation_id(&self.ctx(id))
                .strip_prefix("run-")
                .unwrap()
        )
    }
    fn create(&self) -> Value {
        json!({"agent":"fixture","model_alias":"fixture","permission":"read_only","prompt":"FIRST TASK","disable_startup_context":true,"intent":"fixture"})
    }
    async fn call(&self, id: &str, input: Value) -> Result<ToolOutput> {
        let ctx = self.ctx(id);
        self.ids
            .lock()
            .unwrap()
            .push(crate::execution::invocation_id(&ctx));
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.registry.execute("subagent", input, ctx),
        )
        .await
        .context("Host fixture deadline")?
    }
    async fn terminal(&self, id: &str) -> Result<crate::execution::RunRecord> {
        let store = ExecutionStore::open(self.home.root())?;
        let id = crate::execution::invocation_id(&self.ctx(id));
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            crate::execution::await_terminal(&store, &id, &InterruptSignal::new()),
        )
        .await??;
        store.inspect(&id)?.context("Missing result")
    }
    async fn cleanup(&self) {
        let ids = self.ids.lock().unwrap().clone();
        let store = ExecutionStore::open(self.home.root()).unwrap();
        for id in &ids {
            let _ = crate::execution::request_stop(id, StopCause::HumanCancellation);
        }
        for id in ids {
            if store.inspect(&id).unwrap().is_some() {
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    crate::execution::await_terminal(&store, &id, &InterruptSignal::new()),
                )
                .await;
            }
        }
        idle::clear(self.home.root());
        self.host.pool.disconnect_all().await;
    }
}

#[tokio::test]
async fn hosted_native_child_artifact_parent_inspection_and_source_free_followup() {
    let f = Fixture::new().await;
    let result=std::panic::AssertUnwindSafe(async {
        let child_id=f.child_id("first");let artifact=f.home.root().join("artifacts").join(&child_id).join("research.md");
        f.http.push(Reply::Tool("write",json!({"file_path":artifact,"content":"EXACT ARTIFACT","intent":"fixture artifact"})));
        f.http.push(Reply::Tool("session_outline",json!({"target":"parent","intent":"inspect parent"})));
        f.http.push(Reply::Text("FIRST REPLY"));
        let output=f.call("first",f.create()).await?;
        ensure!(!output.is_error && output.output.contains("FIRST REPLY"),"{output:?}");
        ensure!(std::fs::read_to_string(&artifact)?=="EXACT ARTIFACT","artifact lost");
        let first=Session::load(&child_id)?;let system=first.system_prompt.clone();let resolution=first.isolated_child.as_ref().unwrap().identity.resolution.clone();
        let requests=f.http.requests.lock().unwrap().clone();
        ensure!(!requests[0].to_string().contains("PARENT-PRIVATE-TEXT"),"parent transcript copied at creation");
        ensure!(requests[2].to_string().contains("PARENT-PRIVATE-TEXT"),"on-demand parent inspection absent: {}", requests[2]);
        std::fs::write(f.home.root().join("instructions/model-roster.toml"),"invalid roster = [")?;
        std::fs::write(f.home.root().join("instructions/agents/fixture.md"),"invalid profile")?;
        f.http.push(Reply::Text("FOLLOWUP REPLY"));
        let output=f.call("next",json!({"child_id":child_id,"prompt":"NEXT TASK","permission":"read_write","preset":"special","intent":"followup"})).await?;
        ensure!(output.output.contains("FOLLOWUP REPLY"),"{output:?}");
        let next=Session::load(&child_id)?;
        ensure!(next.system_prompt==system && next.isolated_child.as_ref().unwrap().identity.resolution==resolution,"fixed identity changed");
        ensure!(next.isolated_child.as_ref().unwrap().permission==Permission::ReadWrite,"permission not changed");
        ensure!(f.http.requests.lock().unwrap().len()==4,"duplicate inference");
        ensure!(f.host.provider.model()=="unchanged-parent-model","parent mutated");
        Ok::<_,anyhow::Error>(())
    }).catch_unwind().await;
    f.cleanup().await;
    result.unwrap().unwrap();
}

#[tokio::test]
async fn hosted_fifo_capacity_settings_and_stop_preserve_original_inputs() {
    let f = Fixture::new().await;
    let result=std::panic::AssertUnwindSafe(async {
        let gate=Arc::new(Semaphore::new(0));f.http.push(Reply::Hold(gate.clone()));
        let mut create=f.create();create["run_in_background"]=json!(true);create["notify"]=json!(false);
        let accepted=f.call("busy",create).await?;ensure!(matches!(accepted.source,OutputSource::Acceptance(_)),"not acceptance");
        let child=f.child_id("busy");
        let cap=f.call("overflow",f.create()).await.unwrap_err();ensure!(cap.to_string().contains("capacity"),"{cap:#}");
        let busy=f.call("reject",json!({"child_id":child,"prompt":"REJECTED","intent":"fixture"})).await.unwrap_err();ensure!(busy.to_string().contains("busy"),"{busy:#}");
        for (id,prompt,permission,preset) in [("q1","QUEUED ONE",Some("read_write"),Some("special")),("q2","QUEUED TWO",None,None)] {
            f.call(id,json!({"child_id":child,"prompt":prompt,"permission":permission,"preset":preset,"queue_if_busy":true,"run_in_background":true,"notify":false,"intent":"queue"})).await?;
        }
        ensure!(Session::load(&child)?.isolated_child.unwrap().permission==Permission::ReadOnly,"queue changed active settings");
        std::fs::write(f.home.root().join("instructions/notifications/task-preset.special.md"),"---\nid: task-preset.special\nkind: notification\n---\nLATEST SPECIAL")?;
        f.http.push(Reply::Text("QUEUE ONE REPLY"));f.http.push(Reply::Text("QUEUE TWO REPLY"));gate.add_permits(1);
        for id in ["busy","q1","q2"] {ensure!(f.terminal(id).await?.state==crate::execution::RunState::Completed,"FIFO did not finish");}
        let after=Session::load(&child)?;ensure!(after.isolated_child.as_ref().unwrap().permission==Permission::ReadWrite,"sticky permission lost");
        let wire=f.http.requests.lock().unwrap().clone();ensure!(wire.len()==3,"unexpected inference count");
        ensure!(wire[1].to_string().contains("LATEST SPECIAL"),"preset rendered too early");
        ensure!(wire[1]["messages"].as_array().unwrap().last().unwrap().to_string().contains("QUEUED ONE"),"FIFO order lost");
        let gate=Arc::new(Semaphore::new(0));f.http.push(Reply::Hold(gate));
        f.call("stop",json!({"child_id":child,"prompt":"STOP ME","run_in_background":true,"notify":false,"intent":"fixture"})).await?;
        f.call("cancelled-queue",json!({"child_id":child,"prompt":"PRESERVE UNSENT INPUT","queue_if_busy":true,"run_in_background":true,"notify":false,"intent":"fixture"})).await?;
        crate::execution::request_stop(&crate::execution::invocation_id(&f.ctx("stop")),StopCause::HumanCancellation)?;
        ensure!(f.terminal("stop").await?.state==crate::execution::RunState::Cancelled,"Stop not terminal");
        let queued=f.terminal("cancelled-queue").await?;ensure!(queued.state==crate::execution::RunState::Cancelled,"pending input not cancelled");
        ensure!(ExecutionStore::open(f.home.root())?.invocation_input(&queued.id)?.input["prompt"]=="PRESERVE UNSENT INPUT","queued intent lost");
        Ok::<_,anyhow::Error>(())
    }).catch_unwind().await;
    f.cleanup().await;
    result.unwrap().unwrap();
}

#[tokio::test]
async fn hosted_startup_capture_uses_latest_custom_and_disabled_without_mutating_default() {
    let f = Fixture::new().await;
    let result = std::panic::AssertUnwindSafe(async {
        use crate::startup_context::{StartupContext, StartupSelectionInput};
        let cwd = Path::new(f.parent.working_dir.as_ref().unwrap());
        let file = cwd.join("required.txt");
        std::fs::write(&file, "OLDER DEFAULT")?;
        let engine = StartupContext::new();
        let project = engine.resolve_project(cwd)?;
        let preview =
            engine.preview_selection(&project, [StartupSelectionInput::new("required.txt")]);
        engine.save_project_plan(&project, 0, &preview)?;
        let before = engine.load_project_plan(&project)?.plan().clone();
        std::fs::write(&file, "LATEST DEFAULT")?;
        f.http.push(Reply::Text("DEFAULT REPLY"));
        let mut input = f.create();
        input
            .as_object_mut()
            .unwrap()
            .remove("disable_startup_context");
        f.call("startup-default", input.clone()).await?;
        let default_id = f.child_id("startup-default");
        let default = Session::load(&default_id)?;
        ensure!(
            serde_json::to_string(&default.messages)?.contains("LATEST DEFAULT"),
            "stale default captured"
        );
        let external = f.home.root().join("external.txt");
        std::fs::write(&external, "EXTERNAL CUSTOM")?;
        input["startup_files"] = json!([external]);
        f.http.push(Reply::Text("CUSTOM REPLY"));
        f.call("startup-custom", input.clone()).await?;
        let custom = Session::load(&f.child_id("startup-custom"))?;
        let text = serde_json::to_string(&custom.messages)?;
        ensure!(
            text.contains("EXTERNAL CUSTOM") && !text.contains("LATEST DEFAULT"),
            "custom list did not replace default"
        );
        input["startup_files"] = json!([]);
        f.http.push(Reply::Text("EMPTY REPLY"));
        f.call("startup-empty", input).await?;
        let empty = Session::load(&f.child_id("startup-empty"))?;
        ensure!(
            !serde_json::to_string(&empty.messages)?.contains("LATEST DEFAULT"),
            "empty selection used saved default"
        );
        f.http.push(Reply::Text("DISABLED REPLY"));
        f.call("startup-disabled", f.create()).await?;
        ensure!(
            Session::load(&f.child_id("startup-disabled"))?
                .startup_context
                .is_none(),
            "disabled startup captured context"
        );
        ensure!(
            engine.load_project_plan(&project)?.plan().clone() == before,
            "child changed the saved default"
        );
        std::fs::write(&file, "NOW CHANGED")?;
        f.http.push(Reply::Text("FOLLOWUP"));
        f.call(
            "startup-next",
            json!({"child_id":default_id,"prompt":"NEXT","intent":"fixture"}),
        )
        .await?;
        ensure!(
            serde_json::to_string(&Session::load(&default_id)?.messages)?
                .contains("LATEST DEFAULT"),
            "followup replaced captured bytes"
        );
        Ok::<_, anyhow::Error>(())
    })
    .catch_unwind()
    .await;
    f.cleanup().await;
    result.unwrap().unwrap();
}

#[tokio::test]
async fn hosted_rejections_publish_no_child_and_continuation_does_not_transfer_control() {
    let f = Fixture::new().await;
    let result = std::panic::AssertUnwindSafe(async {
        for (id, key, value) in [
            ("bad-profile", "agent", json!("missing")),
            ("bad-alias", "model_alias", json!("missing")),
            ("bad-effort", "effort", json!("unsupported")),
            ("bad-startup", "startup_files", json!(["missing.txt"])),
        ] {
            let mut input = f.create();
            input[key] = value;
            if key == "startup_files" {
                input
                    .as_object_mut()
                    .unwrap()
                    .remove("disable_startup_context");
            }
            ensure!(f.call(id, input).await.is_err(), "invalid source accepted");
            let child = f.child_id(id);
            ensure!(
                !crate::session::session_exists(&child)
                    && !f.home.root().join("artifacts").join(child).exists(),
                "unpublished child leaked"
            );
        }
        ensure!(
            f.http.requests.lock().unwrap().is_empty(),
            "rejection made a provider request"
        );
        f.http.push(Reply::Text("CHILD REPLY"));
        f.call("owned", f.create()).await?;
        let child = f.child_id("owned");
        let mut split = Session::create(Some(f.parent.id.clone()), None);
        split.inherit_continuation_state_from(&f.parent);
        split.save()?;
        let mut ctx = f.ctx("split-control");
        ctx.session_id = split.id.clone();
        let rejected = f
            .registry
            .execute(
                "subagent",
                json!({"child_id":child,"prompt":"must not send","intent":"fixture"}),
                ctx.clone(),
            )
            .await;
        ensure!(rejected.is_err(), "split inherited child control");
        let owned_run = crate::execution::invocation_id(&f.ctx("owned"));
        ensure!(
            crate::tool::child_policy::authorize_task_control(&ctx, &owned_run, &f.parent.id)
                .is_err(),
            "split inherited Stop control"
        );
        let inspection = crate::session_inspection::agent_inspection_with_context(
            f.home.root().to_path_buf(),
            split.id,
            jcode_tool_types::inspection::InspectionRequest::Outline {
                target: child,
                output_size: None,
            },
            None,
            None,
        )
        .await?;
        ensure!(
            inspection.output.contains("CHILD REPLY"),
            "inherited child history not readable"
        );
        Ok::<_, anyhow::Error>(())
    })
    .catch_unwind()
    .await;
    f.cleanup().await;
    result.unwrap().unwrap();
}

#[tokio::test]
async fn hosted_reload_quiesces_background_children_and_cancels_unstarted_queue() {
    let f = Fixture::new().await;
    let result=std::panic::AssertUnwindSafe(async {
        f.http.push(Reply::Hold(Arc::new(Semaphore::new(0))));
        let mut input=f.create();input["run_in_background"]=json!(true);input["notify"]=json!(false);
        f.call("reload-active",input).await?;
        let child=f.child_id("reload-active");
        f.call("reload-queue",json!({"child_id":child,"prompt":"UNSTARTED RELOAD INPUT","queue_if_busy":true,"run_in_background":true,"notify":false,"intent":"fixture"})).await?;
        crate::execution::await_delegations_for_reload(std::time::Duration::from_secs(15)).await?;
        let active=f.terminal("reload-active").await?;
        ensure!(active.state==crate::execution::RunState::Interrupted && active.stop_cause==Some(StopCause::ReloadQuiescence),"reload lost its cause: {active:?}");
        ensure!(f.terminal("reload-queue").await?.state==crate::execution::RunState::Cancelled,"reload did not cancel pending input");
        Session::load(&child)?.validate_active_agent_profile()?;
        Ok::<_,anyhow::Error>(())
    }).catch_unwind().await;
    f.cleanup().await;
    result.unwrap().unwrap();
}

#[tokio::test]
async fn ordinary_child_followups_reuse_stateful_mcp_until_runtime_eviction() {
    let f = Fixture::new().await;
    let result=std::panic::AssertUnwindSafe(async {
        let script=f.home.root().join("mcp-fixture.py");let starts=f.home.root().join("mcp-starts");
        std::fs::write(&script,r#"import json,os,sys
with open(sys.argv[1],'a') as out: out.write(str(os.getpid())+'\n')
for line in sys.stdin:
 req=json.loads(line)
 if 'id' not in req: continue
 method=req.get('method')
 if method=='initialize': result={'protocolVersion':'2024-11-05','capabilities':{'tools':{}},'serverInfo':{'name':'fixture','version':'1'}}
 elif method=='tools/list': result={'tools':[{'name':'identity','description':'synthetic','inputSchema':{'type':'object','properties':{}}}]}
 else: result={'content':[{'type':'text','text':str(os.getpid())}]}
 print(json.dumps({'jsonrpc':'2.0','id':req['id'],'result':result}),flush=True)
"#)?;
        std::fs::write(f.home.root().join("mcp.json"),serde_json::to_vec(&json!({"mcpServers":{"fixture":{"command":"python3","args":[script,starts],"shared":false,"read_only":true}}}))?)?;
        f.http.push(Reply::Text("FIRST"));f.call("warm",f.create()).await?;
        let child=f.child_id("warm");let first=std::fs::read_to_string(&starts)?;
        f.http.push(Reply::Text("SECOND"));f.call("warm-next",json!({"child_id":child,"prompt":"NEXT","intent":"fixture"})).await?;
        ensure!(std::fs::read_to_string(&starts)?==first,"ordinary followup restarted its stateful MCP");
        idle::clear(f.home.root());
        f.http.push(Reply::Text("AFTER EVICTION"));f.call("cold-next",json!({"child_id":child,"prompt":"NEXT","intent":"fixture"})).await?;
        ensure!(std::fs::read_to_string(&starts)?.lines().count()==2,"evicted runtime was not reconstructed");
        ensure!(Session::load(&child)?.messages.iter().any(|message|serde_json::to_string(message).unwrap().contains("FIRST")),"runtime eviction lost conversation history");
        Ok::<_,anyhow::Error>(())
    }).catch_unwind().await;
    f.cleanup().await;
    result.unwrap().unwrap();
}

#[tokio::test]
async fn child_context_budget_failure_preserves_history_and_never_dispatches() {
    let f = Fixture::new().await;
    let result = std::panic::AssertUnwindSafe(async {
        let large = "synthetic_context ".repeat(500_000);
        let file = f.home.root().join("large-startup.txt");
        std::fs::write(&file, &large)?;
        let mut create = f.create();
        create
            .as_object_mut()
            .unwrap()
            .remove("disable_startup_context");
        create["startup_files"] = json!([file]);
        ensure!(
            f.call("oversize-create", create).await.is_err(),
            "oversize startup was accepted"
        );
        ensure!(
            f.http.requests.lock().unwrap().is_empty(),
            "rejected startup reached the provider"
        );
        ensure!(
            !crate::session::session_exists(&f.child_id("oversize-create")),
            "rejected startup published a child"
        );
        f.http.push(Reply::Text("INITIAL"));
        f.call("budget-child", f.create()).await?;
        let id = f.child_id("budget-child");
        let before = serde_json::to_value(Session::load(&id)?.messages)?;
        ensure!(
            f.call(
                "oversize-followup",
                json!({"child_id":id,"prompt":large,"intent":"fixture"})
            )
            .await
            .is_err(),
            "oversize followup was accepted"
        );
        ensure!(
            serde_json::to_value(Session::load(&id)?.messages)? == before,
            "blocked followup rewrote history"
        );
        ensure!(
            f.http.requests.lock().unwrap().len() == 1,
            "blocked followup made an extra provider request"
        );
        f.http.push(Reply::Text("CONTINUED"));
        f.call(
            "after-budget",
            json!({"child_id":id,"prompt":"continue explicitly","intent":"fixture"}),
        )
        .await?;
        Ok::<_, anyhow::Error>(())
    })
    .catch_unwind()
    .await;
    f.cleanup().await;
    result.unwrap().unwrap();
}

#[tokio::test]
async fn human_child_context_reuses_curator_apply_undo_and_rejects_stale_followup() {
    use crate::protocol::{
        ContextCuratorRunConfig, ContextDraftRequest, ContextMessageRangeSelection, Request,
        ServerEvent,
    };
    let f = Fixture::new().await;
    let result=std::panic::AssertUnwindSafe(async {
        f.http.push(Reply::Text("CHILD REPLY"));f.call("context-child",f.create()).await.unwrap();
        let child=f.child_id("context-child");let before=Session::load(&child).unwrap();
        let parent=serde_json::to_value(Session::load(&f.parent.id).unwrap()).unwrap();
        let service=Arc::new(crate::context::ContextTransactionService::default());
        async fn action(child:&str,request:Request,service:&Arc<crate::context::ContextTransactionService>)->Vec<ServerEvent>{
            let (tx,mut rx)=mpsc::unbounded_channel();
            crate::server::child_context::handle(child.into(),request,service.clone(),InstructionRepositoryService::new(),tx).await.unwrap();
            let mut events=vec![];
            while let Some(event)=tokio::time::timeout(std::time::Duration::from_secs(20),rx.recv()).await.unwrap(){events.push(event);}
            events
        }
        async fn review(child:&str,mut request:ContextDraftRequest,service:&Arc<crate::context::ContextTransactionService>)->ContextDraftRequest {
            request.curator.expected_plan_fingerprint=None;
            let events=action(child,Request::GetContextEditorSnapshot{id:81,page_start:0,page_size:Some(250)},service).await;
            let snapshot=events.iter().find_map(|event|if let ServerEvent::ContextEditorSnapshot{snapshot,..}=event{Some(snapshot)}else{None}).unwrap();
            let events=action(child,Request::PreviewContextCuratorPlan{id:82,expected_context_revision:snapshot.context_revision,expected_transcript_digest:snapshot.transcript_digest,request:request.clone()},service).await;
            let fingerprint=events.iter().find_map(|event|if let ServerEvent::ContextCuratorPlanPreview{preview,..}=event{Some(preview.fingerprint.clone())}else{None}).unwrap_or_else(||panic!("{events:?}"));
            request.curator.expected_plan_fingerprint=Some(fingerprint);request
        }
        let snapshot=action(&child,Request::GetContextEditorSnapshot{id:1,page_start:0,page_size:Some(250)},&service).await;
        assert!(snapshot.iter().any(|event|matches!(event,ServerEvent::ContextEditorSnapshot{snapshot,..} if snapshot.session_id==child)));
        let source=snapshot.iter().find_map(|event|if let ServerEvent::ContextEditorSnapshot{snapshot,..}=event{Some(snapshot)}else{None}).unwrap();
        for directive in &before.isolated_child.as_ref().unwrap().directive_message_ids {
            let rejected=action(&child,Request::PreviewContextRanges{id:80,expected_context_revision:source.context_revision,expected_transcript_digest:source.transcript_digest,ranges:vec![ContextMessageRangeSelection{start_message_id:directive.clone(),end_message_id:directive.clone()}]},&service).await;
            assert!(rejected.iter().any(|event|matches!(event,ServerEvent::ContextRequestRejected{..})),"Active child directive was transformable: {rejected:?}");
        }

        assert_eq!(serde_json::to_value(Session::load(&child).unwrap()).unwrap(),serde_json::to_value(&before).unwrap());
        let request=ContextDraftRequest{summary_ranges:vec![ContextMessageRangeSelection{start_message_id:before.messages[before.messages.len()-2].id.clone(),end_message_id:before.messages.last().unwrap().id.clone()}],reasoning:None,tool_results:vec![],allow_shadowing_active_operations:false,curator:ContextCuratorRunConfig{selection:Some(Default::default()),..Default::default()},authorization:jcode_session_types::StoredContextAuthorization::Manual{initiated_by:Some("human fixture".into())}};
        f.http.push(Reply::Text(r#"{"summary":"Synthetic summary","file_change_digest":"No files changed.","warnings":[]}"#));
        let events=action(&child,Request::PrepareContextDraft{id:2,request:review(&child,request.clone(),&service).await},&service).await;
        let draft=events.iter().find_map(|event|if let ServerEvent::ContextDraftReady{draft,..}=event {Some(draft.identity.draft_id.clone())}else{None}).unwrap_or_else(||panic!("{events:?}"));
        let applied=action(&child,Request::ApplyContextDraft{id:3,draft_id:draft,selected_distillation_ids:None},&service).await;
        let transaction=applied.iter().find_map(|event|if let ServerEvent::ContextTransactionApplied{result,..}=event {Some(result.transaction.id.clone())}else{None}).unwrap_or_else(||panic!("{applied:?}"));
        let edited=Session::load(&child).unwrap();assert_eq!(serde_json::to_value(&edited.messages).unwrap(),serde_json::to_value(&before.messages).unwrap());assert!(edited.context_view.revision>before.context_view.revision);edited.validate_active_agent_profile().unwrap();
        let undone=action(&child,Request::RevertContextTransaction{id:4,transaction_id:transaction},&service).await;
        assert!(undone.iter().any(|event|matches!(event,ServerEvent::ContextTransactionReverted{..})),"{undone:?}");
        f.http.push(Reply::Text(r#"{"summary":"Synthetic next summary","file_change_digest":"No files changed.","warnings":[]}"#));
        let events=action(&child,Request::PrepareContextDraft{id:5,request:review(&child,request,&service).await},&service).await;
        let stale=events.iter().find_map(|event|if let ServerEvent::ContextDraftReady{draft,..}=event {Some(draft.identity.draft_id.clone())}else{None}).unwrap_or_else(||panic!("{events:?}"));
        let calls=f.http.requests.lock().unwrap().len();
        f.http.push(Reply::Text("EXPLICIT FOLLOWUP"));f.call("context-followup",json!({"child_id":child,"prompt":"Continue explicitly","intent":"fixture"})).await.unwrap();
        let changed=Session::load(&child).unwrap();
        let rejected=action(&child,Request::ApplyContextDraft{id:6,draft_id:stale,selected_distillation_ids:None},&service).await;
        assert!(rejected.iter().any(|event|matches!(event,ServerEvent::ContextRequestRejected{..})),"{rejected:?}");
        assert_eq!(serde_json::to_value(Session::load(&child).unwrap()).unwrap(),serde_json::to_value(changed).unwrap());
        assert_eq!(f.http.requests.lock().unwrap().len(),calls+1,"Context apply must not restart child inference");
        assert_eq!(serde_json::to_value(Session::load(&f.parent.id).unwrap()).unwrap(),parent);
        let (tx,_)=mpsc::unbounded_channel();assert!(crate::server::child_context::handle(child,Request::Cancel{id:7},service,InstructionRepositoryService::new(),tx).await.is_err());
    }).catch_unwind().await;
    f.cleanup().await;
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}

#[tokio::test]
async fn human_child_context_rejects_busy_without_borrowing_parent_or_dispatching_model() {
    let f = Fixture::new().await;
    let result = std::panic::AssertUnwindSafe(async {
        let gate = Arc::new(Semaphore::new(0));
        f.http.push(Reply::Hold(gate));
        let mut input = f.create();
        input["run_in_background"] = json!(true);
        f.call("busy-context", input).await.unwrap();
        let child = f.child_id("busy-context");
        let (tx, _rx) = mpsc::unbounded_channel();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            crate::server::child_context::handle(
                child,
                crate::protocol::Request::GetContextEditorSnapshot {
                    id: 1,
                    page_start: 0,
                    page_size: Some(10),
                },
                Arc::new(crate::context::ContextTransactionService::default()),
                InstructionRepositoryService::new(),
                tx,
            ),
        )
        .await
        .unwrap();
        assert!(result.unwrap_err().to_string().contains("busy"));
        let store = ExecutionStore::open(f.home.root()).unwrap();
        let run = crate::execution::invocation_id(&f.ctx("busy-context"));
        let reply = crate::execution::control_transport::control_in_store(
            &store,
            &run,
            crate::execution::ControlOperation::Stop {
                cause: StopCause::HumanCancellation,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            reply,
            crate::execution::ControlReply::Accepted { .. }
        ));
        assert_eq!(
            f.terminal("busy-context").await.unwrap().state,
            jcode_tool_types::RunState::Cancelled
        );
    })
    .catch_unwind()
    .await;
    f.cleanup().await;
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}
