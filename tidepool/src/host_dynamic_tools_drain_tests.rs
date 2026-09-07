use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tidepool_actor::ResidentToolFuture;
use tidepool_tool::CustomToolDeclaration;
use tokio::sync::Semaphore;

const THREAD: &str = "01a05a16-97f5-7722-aa8d-467e01e2e5b4";
const URL: &str = "http://localhost/v1/dynamic-tools";

struct GatedEndpoint {
    tools: Vec<HostedTool>,
    entered: Arc<Semaphore>,
    release: Arc<Semaphore>,
    calls: AtomicUsize,
    completions: AtomicUsize,
}
impl ResidentToolEndpoint for GatedEndpoint {
    fn tools(&self) -> &[HostedTool] {
        &self.tools
    }
    fn instructions(&self) -> Option<&str> {
        None
    }
    fn dispatch_boxed(&self, _: ToolInvocation) -> ResidentToolFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.add_permits(1);
        let release = self.release.clone();
        Box::pin(async move {
            release.acquire().await.unwrap().forget();
            Ok(serde_json::json!({"done":true}))
        })
    }
    fn complete_boxed(
        &self,
        _: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> ResidentToolFuture {
        self.completions.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(serde_json::json!({"completed":true})) })
    }
}
fn endpoint() -> Arc<GatedEndpoint> {
    Arc::new(GatedEndpoint {
        tools: vec![HostedTool::Custom(CustomToolDeclaration {
            name: "haskell".into(),
            description: "Run Haskell".into(),
        })],
        entered: Arc::new(Semaphore::new(0)),
        release: Arc::new(Semaphore::new(0)),
        calls: AtomicUsize::new(0),
        completions: AtomicUsize::new(0),
    })
}
fn client(socket: &std::path::Path) -> reqwest::Client {
    reqwest::Client::builder()
        .unix_socket(socket.to_path_buf())
        .no_proxy()
        .http1_only()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}
fn call_request() -> serde_json::Value {
    serde_json::json!({"protocolVersion":PROTOCOL_VERSION,"threadId":THREAD,"turnId":"turn","callId":"call","contextCallId":"context","namespace":NAMESPACE,"tool":"haskell","arguments":"source"})
}
async fn attach(client: &reqwest::Client) {
    assert_eq!(
        client
            .post(format!("{URL}/session"))
            .json(&serde_json::json!({"protocolVersion":PROTOCOL_VERSION,"threadId":THREAD}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn http_quiesce_preserves_completion_and_drain_retains_blocked_call() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("tools.sock");
    let endpoint = endpoint();
    let service =
        HostDynamicToolService::new(endpoint.clone(), dir.path().join("binding"), None).unwrap();
    let control = service.control();
    let listener = UnixListener::bind(&socket).unwrap();
    let mut server = tokio::spawn(service.serve(listener));
    let keepalive = client(&socket);
    attach(&keepalive).await;
    let active_client = client(&socket);
    let active = tokio::spawn(async move {
        active_client
            .post(format!("{URL}/call"))
            .json(&call_request())
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(5), endpoint.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    control.quiesce();
    control.quiesce();
    for c in [&keepalive, &client(&socket)] {
        assert_eq!(
            c.get(format!("{URL}/registration"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let denied: serde_json::Value = c
            .post(format!("{URL}/call"))
            .json(&call_request())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(denied["success"], false);
        assert_eq!(
            c.post(format!("{URL}/session"))
                .json(&serde_json::json!({"protocolVersion":PROTOCOL_VERSION,"threadId":THREAD}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(c.post(format!("{URL}/completed")).json(&serde_json::json!({"protocolVersion":PROTOCOL_VERSION,"threadId":THREAD,"contextCallId":"context"})).send().await.unwrap().status(),StatusCode::OK);
    }
    assert_eq!(endpoint.calls.load(Ordering::SeqCst), 1);
    assert_eq!(endpoint.completions.load(Ordering::SeqCst), 2);
    control.drain();
    control.quiesce(); // Cannot reopen.
    assert!(tokio::time::timeout(Duration::from_millis(50), &mut server)
        .await
        .is_err());
    endpoint.release.add_permits(1);
    assert_eq!(active.await.unwrap()["success"], true);
    tokio::time::timeout(Duration::from_secs(5), &mut server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(client(&socket)
        .get(format!("{URL}/registration"))
        .send()
        .await
        .is_err());
}

#[tokio::test]
async fn http_idle_keepalive_connection_does_not_prevent_drain() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("tools.sock");
    let service =
        HostDynamicToolService::new(endpoint(), dir.path().join("binding"), None).unwrap();
    let control = service.control();
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(service.serve(listener));
    let c = client(&socket);
    c.get(format!("{URL}/registration"))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let _idle = tokio::net::UnixStream::connect(&socket).await.unwrap();
    control.quiesce();
    control.drain();
    control.drain();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn http_disconnected_client_does_not_authorize_effect_retirement() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("tools.sock");
    let endpoint = endpoint();
    let service =
        HostDynamicToolService::new(endpoint.clone(), dir.path().join("binding"), None).unwrap();
    let control = service.control();
    let listener = UnixListener::bind(&socket).unwrap();
    let mut server = tokio::spawn(service.serve(listener));
    let c = client(&socket);
    attach(&c).await;
    let active = tokio::spawn(async move {
        c.post(format!("{URL}/call"))
            .json(&call_request())
            .send()
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), endpoint.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    active.abort();
    assert!(active.await.unwrap_err().is_cancelled());
    control.quiesce();
    assert_eq!(endpoint.calls.load(Ordering::SeqCst), 1);
    // Release the endpoint explicitly; client cancellation is never used as an
    // endpoint completion signal. This fixture is not a resident-effect proof.
    endpoint.release.add_permits(1);
    control.drain();
    tokio::time::timeout(Duration::from_secs(5), &mut server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(endpoint.calls.load(Ordering::SeqCst), 1);
}

struct FailingSealEndpoint {
    inner: Arc<GatedEndpoint>,
    seals: AtomicUsize,
    release_seal: Arc<Semaphore>,
}
impl ResidentToolEndpoint for FailingSealEndpoint {
    fn tools(&self) -> &[HostedTool] {
        self.inner.tools()
    }
    fn instructions(&self) -> Option<&str> {
        None
    }
    fn dispatch_boxed(&self, invocation: ToolInvocation) -> ResidentToolFuture {
        self.inner.dispatch_boxed(invocation)
    }
    fn complete_boxed(
        &self,
        boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> ResidentToolFuture {
        self.inner.complete_boxed(boundary)
    }
    fn seal_hosted_work_boxed(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<tidepool_actor::HostedWorkSeal, ResidentToolError>,
                > + Send
                + 'static,
        >,
    > {
        self.seals.fetch_add(1, Ordering::SeqCst);
        let release = self.release_seal.clone();
        Box::pin(async move {
            release.acquire().await.unwrap().forget();
            Err(ResidentToolError::Unavailable(
                "explicit seal failure".into(),
            ))
        })
    }
}

async fn assert_quiesced_but_completion_available(c: &reqwest::Client) {
    let denied: serde_json::Value = c
        .post(format!("{URL}/call"))
        .json(&call_request())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(denied["success"], false);
    assert_eq!(
        c.post(format!("{URL}/session"))
            .json(&serde_json::json!({"protocolVersion":PROTOCOL_VERSION,"threadId":THREAD}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(c.post(format!("{URL}/completed"))
        .json(&serde_json::json!({"protocolVersion":PROTOCOL_VERSION,"threadId":THREAD,"contextCallId":"context"}))
        .send().await.unwrap().status(), StatusCode::OK);
}

#[tokio::test]
async fn http_seal_unsupported_quiesces_before_poll_without_draining() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("tools.sock");
    let endpoint = endpoint(); // Uses the real trait's unsupported default.
    let service =
        HostDynamicToolService::new(endpoint.clone(), dir.path().join("binding"), None).unwrap();
    let control = service.control();
    let mut server = tokio::spawn(service.serve(UnixListener::bind(&socket).unwrap()));
    let c = client(&socket);
    attach(&c).await;
    let seal =
        control.quiesce_and_seal(tidepool_actor::ActorRef::first(tidepool_actor::ActorId(41)));
    assert_quiesced_but_completion_available(&c).await; // Future not polled yet.
    assert!(matches!(
        seal.await,
        Err(HostToolSealError::Endpoint(ResidentToolError::Unavailable(
            _
        )))
    ));
    assert_quiesced_but_completion_available(&client(&socket)).await;
    assert_eq!(endpoint.calls.load(Ordering::SeqCst), 0);
    assert_eq!(endpoint.completions.load(Ordering::SeqCst), 2);
    assert!(tokio::time::timeout(Duration::from_millis(30), &mut server)
        .await
        .is_err());
    control.drain();
    tokio::time::timeout(Duration::from_secs(5), &mut server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn http_seal_timeout_retains_single_future_and_failure_keeps_completion() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("tools.sock");
    let endpoint = Arc::new(FailingSealEndpoint {
        inner: endpoint(),
        seals: AtomicUsize::new(0),
        release_seal: Arc::new(Semaphore::new(0)),
    });
    let service =
        HostDynamicToolService::new(endpoint.clone(), dir.path().join("binding"), None).unwrap();
    let control = service.control();
    let mut server = tokio::spawn(service.serve(UnixListener::bind(&socket).unwrap()));
    let c = client(&socket);
    attach(&c).await;
    let mut seal =
        control.quiesce_and_seal(tidepool_actor::ActorRef::first(tidepool_actor::ActorId(42)));
    assert_eq!(endpoint.seals.load(Ordering::SeqCst), 0);
    assert_quiesced_but_completion_available(&c).await;
    for _ in 0..2 {
        assert!(tokio::time::timeout(Duration::from_millis(30), &mut seal)
            .await
            .is_err());
        assert_eq!(endpoint.seals.load(Ordering::SeqCst), 1);
    }
    endpoint.release_seal.add_permits(1);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), &mut seal)
            .await
            .unwrap(),
        Err(HostToolSealError::Endpoint(ResidentToolError::Unavailable(
            _
        )))
    ));
    assert_eq!(endpoint.seals.load(Ordering::SeqCst), 1);
    assert_quiesced_but_completion_available(&client(&socket)).await;
    assert_eq!(endpoint.inner.calls.load(Ordering::SeqCst), 0);
    assert_eq!(endpoint.inner.completions.load(Ordering::SeqCst), 2);
    assert!(tokio::time::timeout(Duration::from_millis(30), &mut server)
        .await
        .is_err());
    control.drain();
    tokio::time::timeout(Duration::from_secs(5), &mut server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn http_seal_already_draining_never_invokes_endpoint() {
    for serve_first in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("tools.sock");
        let endpoint = Arc::new(FailingSealEndpoint {
            inner: endpoint(),
            seals: AtomicUsize::new(0),
            release_seal: Arc::new(Semaphore::new(0)),
        });
        let service =
            HostDynamicToolService::new(endpoint.clone(), dir.path().join("binding"), None)
                .unwrap();
        let control = service.control();
        let listener = UnixListener::bind(&socket).unwrap();
        let mut server = if serve_first {
            let server = tokio::spawn(service.serve(listener));
            attach(&client(&socket)).await;
            control.drain();
            server
        } else {
            control.drain();
            tokio::spawn(service.serve(listener))
        };
        let seal =
            control.quiesce_and_seal(tidepool_actor::ActorRef::first(tidepool_actor::ActorId(43)));
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), seal)
                .await
                .unwrap(),
            Err(HostToolSealError::AlreadyDraining)
        ));
        assert_eq!(endpoint.seals.load(Ordering::SeqCst), 0);
        assert!(!control.admits(AdmissionKind::CompletionOrRead));
        tokio::time::timeout(Duration::from_secs(5), &mut server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

mod actual_seal {
    // Real authored dispatch may compile on a cold extractor; pure transport
    // cases above retain their short timeout.
    fn client(socket: &std::path::Path) -> reqwest::Client {
        reqwest::Client::builder()
            .unix_socket(socket.to_path_buf())
            .no_proxy()
            .http1_only()
            .timeout(std::time::Duration::from_secs(90))
            .build()
            .unwrap()
    }
    use super::*;
    use tidepool_actor::{
        ActorWorkbenchSource, Incarnation, LocalResidentDeployment, ResidentForest,
    };
    use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
    use tidepool_runtime::session::{
        insert_preamble_imports, resident_workbench_templates, run_turn, ModuleEnv,
        ResidentSession, SessionLib, TurnRequest, TurnResult,
    };
    use tidepool_testing::eval_harness;
    #[derive(Clone, Default)]
    struct TestSink;
    impl tidepool_runtime::session::OutputSink for TestSink {
        fn drain(&self) -> Vec<String> {
            Vec::new()
        }
        fn snapshot(&self) -> Vec<String> {
            Vec::new()
        }
    }
    struct NoHandlers;
    impl tidepool_effect::dispatch::DispatchEffect<TestSink> for NoHandlers {
        fn dispatch(
            &mut self,
            _: &tidepool_eval::Value,
            _: &tidepool_effect::dispatch::EffectContext<'_, TestSink>,
        ) -> Result<Option<tidepool_effect::Response>, tidepool_effect::error::EffectError>
        {
            Ok(None)
        }
    }

    struct DelegatedEndpoint {
        real: Arc<dyn ResidentToolEndpoint>,
        delay_dispatch: std::sync::atomic::AtomicBool,
        entered: Arc<Semaphore>,
        release_dispatch: Arc<Semaphore>,
        release_seal: Arc<Semaphore>,
        seals: AtomicUsize,
    }
    impl ResidentToolEndpoint for DelegatedEndpoint {
        fn tools(&self) -> &[HostedTool] {
            self.real.tools()
        }
        fn instructions(&self) -> Option<&str> {
            self.real.instructions()
        }
        fn dispatch_boxed(&self, invocation: ToolInvocation) -> ResidentToolFuture {
            let real = self.real.clone();
            let gate = self.release_dispatch.clone();
            let delayed = self.delay_dispatch.load(Ordering::SeqCst);
            if delayed {
                self.entered.add_permits(1);
            }
            Box::pin(async move {
                if delayed {
                    gate.acquire().await.unwrap().forget();
                }
                real.dispatch_boxed(invocation).await
            })
        }
        fn complete_boxed(
            &self,
            boundary: tidepool_runtime::session::WorkbenchForkBoundary,
        ) -> ResidentToolFuture {
            self.real.complete_boxed(boundary)
        }
        fn reattach_boxed(&self) -> ResidentToolFuture {
            self.real.reattach_boxed()
        }
        fn seal_hosted_work_boxed(
            &self,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<tidepool_actor::HostedWorkSeal, ResidentToolError>,
                    > + Send
                    + 'static,
            >,
        > {
            self.seals.fetch_add(1, Ordering::SeqCst);
            let real = self.real.clone();
            let gate = self.release_seal.clone();
            Box::pin(async move {
                gate.acquire().await.unwrap().forget();
                real.seal_hosted_work_boxed().await
            })
        }
    }
    #[tokio::test]
    async fn http_actual_resident_seal_identity_late_dispatch_and_completion() {
        eval_harness::require_extract();
        let declarations = [
            tidepool_mcp::agent_session_decl(),
            tidepool_mcp::actor_local_decl(),
        ];
        let effects = tidepool_mcp::ensure_effects_module(&declarations).unwrap();
        let mut include = effects.include_paths().to_vec();
        include.push(eval_harness::prelude_path());
        include.push(crate::haskell_sources::ensure_shoal_haskell().unwrap());
        let preamble = insert_preamble_imports(
            &tidepool_mcp::build_preamble(&declarations, false),
            "Tidepool.Actors.Internal.ShoalDriver",
        );
        let templates = resident_workbench_templates(&preamble, "RootEffects", "");
        let include_refs: Vec<_> = include.iter().map(std::path::PathBuf::as_path).collect();
        let root = tempfile::tempdir().unwrap();
        let compile = |text: &str, gen| match run_turn(TurnRequest {
            turn_text: text,
            templates: &templates,
            include: &include_refs,
            session_root: root.path(),
            inject_modules: &[],
            gen,
            verdict: None,
            target: None,
        })
        .unwrap()
        {
            TurnResult::Expr { compiled, .. } => Arc::new(compiled),
            _ => panic!("expected compiled program"),
        };
        let boot = compile("pure (0 :: Int)", 1);
        let program = compile("rootDriver", 2);
        let session = tidepool_repr::SessionId((u64::from(std::process::id()) << 32) | 917);
        let lib = SessionLib::open(session, root.path(), ModuleEnv::standalone_default())
            .unwrap()
            .with_validation_include(include.clone());
        let mut machine = ResidentSession::bootstrap(
            &boot.expr,
            boot.table.clone(),
            NoHandlers,
            TestSink,
            include.clone(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        )
        .unwrap();
        machine.set_effect_execution(
            EffectRunPolicy::SuspendAll,
            LivePayloadPolicy::HASKELL_EFFECT_VALUE,
        );
        let (forest, mut events) = ResidentForest::new(
            ActorWorkbenchSource::new(preamble.clone(), include.clone()),
            session,
            machine,
            None,
            Incarnation::FIRST,
        );

        let forest = Arc::new(forest);
        let launch_forest = forest.clone();
        let mut startup = tokio::spawn(async move {
            launch_forest
                .new_program_root(
                    "http-seal".into(),
                    tidepool_actor::EffectiveRole::root(),
                    program,
                )
                .await
        });
        let mut hosted_task = None;
        let mut server_task = None;
        let mut late_task = None;
        let mut retained_control = None;
        let mut retained_endpoint: Option<Arc<DelegatedEndpoint>> = None;
        let mut late_joined = false;
        let mut startup_joined = false;
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("tools.sock");
        let exercise = AssertUnwindSafe(async {
        let startup_result = tokio::time::timeout(Duration::from_secs(90), &mut startup).await.expect("root startup bounded");
        startup_joined = true;
        let (actor, hosted) = startup_result.unwrap().unwrap();
        hosted_task = Some(hosted);
        let LocalResidentDeployment::PolicyInstalled(installation) = tokio::time::timeout(Duration::from_secs(30), events.recv()).await.unwrap().unwrap()
        else {
            panic!("expected actual policy");
        };
        let endpoint = Arc::new(DelegatedEndpoint {
            real: installation.policy,
            delay_dispatch: std::sync::atomic::AtomicBool::new(false),
            entered: Arc::new(Semaphore::new(0)),
            release_dispatch: Arc::new(Semaphore::new(0)),
            release_seal: Arc::new(Semaphore::new(0)),
            seals: AtomicUsize::new(0),
        });
        retained_endpoint = Some(endpoint.clone());
        let service =
            HostDynamicToolService::new(endpoint.clone(), dir.path().join("binding"), None)
                .unwrap();
        let control = service.control();
        retained_control = Some(control.clone());
        server_task = Some(tokio::spawn(service.serve(UnixListener::bind(&socket).unwrap())));
        let c = client(&socket);
        attach(&c).await;
        let mut request = call_request();
        request["arguments"] = serde_json::json!("40 + 2 :: Int");
        let result: serde_json::Value = c
            .post(format!("{URL}/call"))
            .json(&request)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(result["success"], true, "{result}");
        let items = result["contentItems"].as_array().expect("typed content items");
        assert_eq!(items.len(), 1, "{result}");
        assert_eq!(items[0]["type"], "inputText", "{result}");
        assert_eq!(items[0]["text"].as_str().unwrap().lines().collect::<Vec<_>>(), ["42"], "{result}");
        endpoint.delay_dispatch.store(true, Ordering::SeqCst);
        request["callId"] = serde_json::json!("late");
        request["contextCallId"] = serde_json::json!("late");
        let late_client = client(&socket);
        late_task = Some(tokio::spawn(async move {
            late_client
                .post(format!("{URL}/call"))
                .json(&request)
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap()
        }));
        tokio::time::timeout(Duration::from_secs(5), endpoint.entered.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        let mut seal = control.quiesce_and_seal(actor.identity());
        assert!(tokio::time::timeout(Duration::from_millis(30), &mut seal)
            .await
            .is_err());
        assert_eq!(endpoint.seals.load(Ordering::SeqCst), 1);
        endpoint.release_seal.add_permits(1);
        let proof = tokio::time::timeout(Duration::from_secs(30), &mut seal)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(proof.actor(), actor.identity());
        assert_eq!(endpoint.seals.load(Ordering::SeqCst), 1);
        endpoint.release_dispatch.add_permits(1);
        let late_result = tokio::time::timeout(Duration::from_secs(30), late_task.as_mut().unwrap()).await.unwrap();
        late_joined = true;
        let denied_late = late_result.unwrap();
        assert_eq!(denied_late["success"], false);
        assert!(
            denied_late
                .to_string()
                .contains("hosted work admission is sealed"),
            "{denied_late}"
        );
        let completion = serde_json::json!({"protocolVersion":PROTOCOL_VERSION,"threadId":THREAD,"contextCallId":"context"});
        let mut wrong = completion.clone();
        wrong["threadId"] = serde_json::json!("foreign");
        assert_eq!(
            c.post(format!("{URL}/completed"))
                .json(&wrong)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            c.post(format!("{URL}/completed"))
                .json(&completion)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        for expected in [
            tidepool_actor::ActorRef {
                id: tidepool_actor::ActorId(actor.identity().id.0 + 1),
                ..actor.identity()
            },
            tidepool_actor::ActorRef {
                incarnation: Incarnation(actor.identity().incarnation.0 + 1),
                ..actor.identity()
            },
        ] {
            endpoint.release_seal.add_permits(1);
            let mut foreign_seal = control.quiesce_and_seal(expected);
            match tokio::time::timeout(Duration::from_secs(30), &mut foreign_seal).await.unwrap() {
                Err(HostToolSealError::ForeignActor {
                    expected: e,
                    actual,
                }) => {
                    assert_eq!(e, expected);
                    assert_eq!(actual, actor.identity());
                }
                other => panic!("expected foreign seal error: {other:?}"),
            }
        }
        }).catch_unwind().await;
        // Test-local recovery, not production cleanup evidence. Always release
        // our artificial gates before requesting real owners to stop.
        if let Some(endpoint) = retained_endpoint {
            endpoint.release_dispatch.add_permits(8);
            endpoint.release_seal.add_permits(8);
        }
        if let Some(control) = retained_control {
            control.drain();
        }
        let mut cleanup_failures = Vec::new();
        if let Some(task) = late_task.as_mut().filter(|_| !late_joined) {
            if finish_test_task(task).await.is_none() {
                cleanup_failures.push("late HTTP task");
            }
        }
        if !startup_joined {
            match tokio::time::timeout(Duration::from_secs(30), &mut startup).await {
                Ok(Ok(Ok((_, hosted)))) => hosted_task = Some(hosted),
                Ok(_) => cleanup_failures.push("root startup failed"),
                Err(_) => {
                    startup.abort();
                    let _ = tokio::time::timeout(Duration::from_secs(5), &mut startup).await;
                    cleanup_failures.push("root startup aborted without completion proof");
                }
            }
        }
        if tokio::time::timeout(Duration::from_secs(45), forest.shutdown())
            .await
            .is_err()
        {
            cleanup_failures.push("forest shutdown");
        }
        if let Some(task) = server_task.as_mut() {
            if !matches!(finish_test_task(task).await, Some(Ok(()))) {
                cleanup_failures.push("HTTP server");
            }
        }
        if let Some(task) = hosted_task.as_mut() {
            if finish_test_task(task).await.is_none() {
                cleanup_failures.push("hosted actor");
            }
        }
        if let Err(panic) = exercise {
            eprintln!("test cleanup failures after original panic: {cleanup_failures:?}");
            std::panic::resume_unwind(panic);
        }
        assert!(
            cleanup_failures.is_empty(),
            "test cleanup failed: {cleanup_failures:?}"
        );
    }

    async fn finish_test_task<T>(task: &mut tokio::task::JoinHandle<T>) -> Option<T> {
        match tokio::time::timeout(Duration::from_secs(30), &mut *task).await {
            Ok(result) => result.ok(),
            Err(_) => {
                task.abort();
                let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
                None // emergency abort is not a successful drain
            }
        }
    }
}
