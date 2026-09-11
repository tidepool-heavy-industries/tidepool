use super::*;
use tidepool_actor::{ResidentToolEndpoint, ResidentToolError, ResidentToolFuture};
use tidepool_agent::{
    AgentBackendError, InteractiveAgentCommand, InteractiveAgentSpec, InteractiveFuture,
    InteractiveInputError, InteractiveNativeToolPolicy, InteractivePolicyMount,
};
use tidepool_runtime::session::ModuleEnv;
use tidepool_tool::{HostedTool, ToolInvocation};
use tokio::sync::Semaphore;

const URL: &str = "http://localhost/v1/dynamic-tools";
const THREAD: &str = "01a05a16-97f5-7722-aa8d-467e01e2e5b4";
fn protocol() -> u32 {
    tidepool_agent::HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION
}
fn client(socket: &Path) -> reqwest::Client {
    reqwest::Client::builder()
        .unix_socket(socket.to_path_buf())
        .no_proxy()
        .http1_only()
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap()
}
async fn attach(client: &reqwest::Client) {
    let response = client
        .post(format!("{URL}/session"))
        .json(&serde_json::json!({"protocolVersion":protocol(),"threadId":THREAD}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
}
async fn call(client: &reqwest::Client, source: &str, id: &str) -> serde_json::Value {
    client
        .post(format!("{URL}/call"))
        .json(&serde_json::json!({
            "protocolVersion":protocol(), "threadId":THREAD, "turnId":id,
            "callId":id, "contextCallId":id, "namespace":"tidepool_actor",
            "tool":"haskell", "arguments":source,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}
async fn completed(client: &reqwest::Client, id: &str) -> reqwest::Response {
    client
        .post(format!("{URL}/completed"))
        .json(&serde_json::json!({
            "protocolVersion":protocol(),"threadId":THREAD,"contextCallId":id,
        }))
        .send()
        .await
        .unwrap()
}
fn cancelled() -> ActorTerminal {
    ActorTerminal {
        kind: ActorExitKind::Cancelled,
        summary: "fixture shutdown".into(),
    }
}
fn limit() -> Duration {
    Duration::from_secs(120)
}

struct HttpFixture {
    _directory: tempfile::TempDir,
    owners: InteractiveOwners,
    slot: HostedSlot,
    owner: HostedOwner,
    client: reqwest::Client,
}
impl HttpFixture {
    async fn start(actor: LocalActorRef, endpoint: Arc<dyn ResidentToolEndpoint>) -> Self {
        Self::with_endpoint(actor, Some(endpoint)).await
    }
    async fn canonical(actor: LocalActorRef) -> Self {
        Self::with_endpoint(actor, None).await
    }
    async fn canonical_requiring_input_seal(actor: LocalActorRef) -> Self {
        Self::with_endpoint_mode(actor, None, true).await
    }
    async fn with_endpoint(
        actor: LocalActorRef,
        endpoint: Option<Arc<dyn ResidentToolEndpoint>>,
    ) -> Self {
        Self::with_endpoint_mode(actor, endpoint, false).await
    }
    async fn with_endpoint_mode(
        actor: LocalActorRef,
        endpoint: Option<Arc<dyn ResidentToolEndpoint>>,
        require_input_seal: bool,
    ) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("http.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let owners: InteractiveOwners = Arc::new(Mutex::new(HashMap::new()));
        let exact = actor.identity();
        owners.lock().insert(
            exact,
            InteractiveApplicationOwner {
                supervisor: None,
                creator_workspace: None,
                cancel: None,
                native_retirement: Default::default(),
                pane: Arc::new(Mutex::new(None)),
                fork_gate: None,
                custody: None,
                scoped_retention: None,
                hosted: Arc::new(Mutex::new(None)),
                launch: HostLaunchState::Pending,
                pending_activations: Vec::new(),
                terminal: None,
                retirement: Arc::new(Mutex::new(None)),
            },
        );
        let slot = owners.lock().get(&exact).unwrap().hosted.clone();
        let binding = directory.path().join("binding");
        let owner = match endpoint {
            Some(endpoint) => start_untrusted(&slot, actor, endpoint, binding, listener),
            None => start(&slot, actor, binding, None, listener),
        }
        .unwrap();
        if !require_input_seal {
            // Older hosted-work tests isolate the resident/HTTP boundary. The
            // production constructor itself remains fail-closed.
            exempt_input_seal_for_fixture(&owner).await;
        }
        assert!(Arc::ptr_eq(slot.lock().as_ref().unwrap(), &owner));
        let client = client(&socket);
        attach(&client).await;
        Self {
            _directory: directory,
            owners,
            slot,
            owner,
            client,
        }
    }
    // Fixture-only disposal after negative assertions. This does not manufacture
    // resident evidence or turn the production retained state into success.
    async fn dispose_http(&self) {
        let mut owner = self.owner.lock().await;
        owner.control.drain();
        let service = owner.service.take();
        drop(owner);
        // A failing negative assertion may follow an unexpected production
        // drain that already consumed the task. Otherwise join the original,
        // including a task that finished but has not yet been consumed.
        if let Some(service) = service {
            tokio::time::timeout(limit(), service)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
    }
    async fn finish(&self) -> HostedObservation {
        assert_eq!(self.owners.lock().len(), 1);
        observe(&self.owner, CompletionBoundary::AbortForShutdown, limit()).await
    }
}

struct SealBackend {
    calls: std::sync::atomic::AtomicUsize,
    entered: Arc<Semaphore>,
    release: Option<Arc<Semaphore>>,
    result: SealResult,
}

#[derive(Clone, Copy)]
enum SealResult {
    Applied,
    Unknown,
    Failed,
}

impl SealBackend {
    fn immediate(result: SealResult) -> Arc<Self> {
        Arc::new(Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
            entered: Arc::new(Semaphore::new(0)),
            release: None,
            result,
        })
    }
    fn held(entered: Arc<Semaphore>, release: Arc<Semaphore>) -> Arc<Self> {
        Arc::new(Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
            entered,
            release: Some(release),
            result: SealResult::Applied,
        })
    }
}
impl InteractiveAgentBackend for SealBackend {
    fn seal_input_producer<'a>(
        &'a self,
        thread: &'a QueueReadyThread,
        producer: &'a InputProducerId,
    ) -> tidepool_agent::InputProducerControlFuture<'a> {
        assert_eq!(thread.id().0, THREAD);
        assert_eq!(producer.as_str(), "fixture-producer");
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let entered = self.entered.clone();
        let release = self.release.clone();
        let result = self.result;
        Box::pin(async move {
            entered.add_permits(1);
            if let Some(release) = release {
                release.acquire().await.unwrap().forget();
            }
            match result {
                SealResult::Applied => Ok(InputProducerControlOutcome::Applied),
                SealResult::Unknown => Ok(InputProducerControlOutcome::Unknown),
                SealResult::Failed => Err(InteractiveInputError::Unconfirmed(
                    "native seal reply lost".into(),
                )),
            }
        })
    }
    fn prepare_native_tool_policy(
        &self,
        _: InteractiveNativeToolPolicy,
        _: &Path,
    ) -> Result<Vec<InteractivePolicyMount>, AgentBackendError> {
        unreachable!("seal fixture does not launch a process")
    }
    fn render(
        &self,
        _: &InteractiveAgentSpec,
    ) -> Result<InteractiveAgentCommand, AgentBackendError> {
        unreachable!("seal fixture does not render a command")
    }
    fn push<'a>(
        &'a self,
        _: &'a str,
        _: &'a QueueReadyThread,
        _: &'a str,
    ) -> InteractiveFuture<'a, ()> {
        unreachable!("seal fixture does not push input")
    }
    fn archive<'a>(&'a self, _: &'a str, _: &'a QueueReadyThread) -> InteractiveFuture<'a, ()> {
        unreachable!("seal fixture does not archive")
    }
}

async fn queue_ready_thread(directory: &Path) -> QueueReadyThread {
    let binding = directory.join("native-binding.json");
    tidepool_agent::accept_interactive_session_binding(
        &binding,
        protocol(),
        BackendThreadId(THREAD.into()),
        None,
    )
    .await
    .unwrap();
    tidepool_agent::read_interactive_binding(&binding)
        .await
        .unwrap()
}
fn producer() -> InputProducerId {
    InputProducerId::new("fixture-producer".into()).unwrap()
}
fn confirmed_http(observation: HostedObservation, actor: ActorRef) {
    let HostedObservation::Observed { resident, http, .. } = observation else {
        panic!("cleanup still pending");
    };
    let ResidentObservation::Accounted(cleanup) = resident else {
        panic!("resident evidence missing: {resident:?}")
    };
    assert_eq!(cleanup.actor(), actor);
    assert!(cleanup.is_confirmed(), "{cleanup:?}");
    assert!(matches!(http, HttpObservation::Drained), "{http:?}");
}

/// Scheduling decorator only: all accepted tool work, completion callbacks and
/// successful seals execute on the real resident endpoint. No evidence is made here.
struct HeldEndpoint {
    inner: Arc<dyn ResidentToolEndpoint>,
    seal_entered: Semaphore,
    seal_release: Arc<Semaphore>,
    completion_entered: Arc<Semaphore>,
    completion_release: Arc<Semaphore>,
    hold_seal: bool,
    hold_completion: bool,
    seals: std::sync::atomic::AtomicUsize,
}
impl ResidentToolEndpoint for HeldEndpoint {
    fn tools(&self) -> &[HostedTool] {
        self.inner.tools()
    }
    fn instructions(&self) -> Option<&str> {
        self.inner.instructions()
    }
    fn dispatch_boxed(&self, invocation: ToolInvocation) -> ResidentToolFuture {
        self.inner.dispatch_boxed(invocation)
    }
    fn complete_boxed(
        &self,
        boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> ResidentToolFuture {
        let inner = self.inner.clone();
        let entered = self.completion_entered.clone();
        let release = self.completion_release.clone();
        let hold = self.hold_completion;
        Box::pin(async move {
            let result = inner.complete_boxed(boundary).await;
            if hold {
                entered.add_permits(1);
                release.acquire().await.unwrap().forget();
            }
            result
        })
    }
    fn seal_hosted_work_boxed(
        &self,
    ) -> BoxFuture<'static, Result<HostedWorkSeal, ResidentToolError>> {
        self.seals.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.seal_entered.add_permits(1);
        let inner = self.inner.clone();
        let release = self.seal_release.clone();
        let hold = self.hold_seal;
        Box::pin(async move {
            if hold {
                release.acquire().await.unwrap().forget();
            }
            inner.seal_hosted_work_boxed().await
        })
    }
}
fn held(inner: Arc<dyn ResidentToolEndpoint>, seal: bool, completion: bool) -> Arc<HeldEndpoint> {
    Arc::new(HeldEndpoint {
        inner,
        seal_entered: Semaphore::new(0),
        seal_release: Arc::new(Semaphore::new(0)),
        completion_entered: Arc::new(Semaphore::new(0)),
        completion_release: Arc::new(Semaphore::new(0)),
        hold_seal: seal,
        hold_completion: completion,
        seals: std::sync::atomic::AtomicUsize::new(0),
    })
}
async fn entered(semaphore: &Semaphore) {
    tokio::time::timeout(limit(), semaphore.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
}

#[tokio::test]
async fn production_retirement_requires_native_input_fence() {
    let campaign = test_campaign::TestCampaign::start().await;
    let fixture = HttpFixture::canonical_requiring_input_seal(campaign.actor.clone()).await;
    let observation = fixture.finish().await;
    assert!(matches!(
        observation,
        HostedObservation::Observed {
            input_seal: InputSealObservation::Required,
            seal: SealObservation::Pending,
            resident: ResidentObservation::Pending,
            http: HttpObservation::Pending,
        }
    ));
    assert!(campaign.actor.terminal().get().is_none());
    assert!(matches!(
        stop_retired_tool_service(campaign.actor.identity(), &mut fixture.owner.clone()).await,
        CleanupComponentOutcome::Failed { .. }
    ));
    fixture.dispose_http().await;
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn explicit_pre_admission_path_can_retire_without_a_producer() {
    let campaign = test_campaign::TestCampaign::start().await;
    let actor = campaign.actor.identity();
    let fixture = HttpFixture::canonical_requiring_input_seal(campaign.actor.clone()).await;
    confirm_no_input_producer(&fixture.owner).await.unwrap();
    let observation = fixture.finish().await;
    assert!(matches!(
        observation,
        HostedObservation::Observed {
            input_seal: InputSealObservation::NoProducer,
            ..
        }
    ));
    confirmed_http(observation, actor);
    assert_eq!(
        stop_retired_tool_service(actor, &mut fixture.owner.clone()).await,
        CleanupComponentOutcome::Completed
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn native_input_seal_failure_retains_hosted_completion_and_http_owner() {
    let campaign = test_campaign::TestCampaign::start().await;
    let fixture = HttpFixture::canonical_requiring_input_seal(campaign.actor.clone()).await;
    assert_eq!(
        call(&fixture.client, "let answer = 11 :: Int", "input-seal").await["success"],
        true
    );
    let backend = SealBackend::immediate(SealResult::Failed);
    begin_input_seal(
        &fixture.owner,
        backend.clone(),
        queue_ready_thread(fixture._directory.path()).await,
        producer(),
    )
    .await
    .unwrap();
    assert_eq!(
        completed(&fixture.client, "input-seal").await.status(),
        reqwest::StatusCode::OK,
        "input producer sealing is not hosted-call completion acknowledgement"
    );
    let observation = fixture.finish().await;
    assert!(matches!(
        observation,
        HostedObservation::Observed {
            input_seal: InputSealObservation::Unconfirmed(_),
            seal: SealObservation::Pending,
            resident: ResidentObservation::Pending,
            http: HttpObservation::Pending,
            ..
        }
    ));
    assert!(campaign.actor.terminal().get().is_none());
    assert_eq!(backend.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(matches!(
        stop_retired_tool_service(campaign.actor.identity(), &mut fixture.owner.clone()).await,
        CleanupComponentOutcome::Failed { .. }
    ));
    fixture.dispose_http().await;
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn unknown_native_input_seal_remains_distinct_from_hosted_call_completion() {
    let campaign = test_campaign::TestCampaign::start().await;
    let fixture = HttpFixture::canonical_requiring_input_seal(campaign.actor.clone()).await;
    assert_eq!(
        call(&fixture.client, "let answer = 12 :: Int", "unknown-seal").await["success"],
        true
    );
    let backend = SealBackend::immediate(SealResult::Unknown);
    begin_input_seal(
        &fixture.owner,
        backend.clone(),
        queue_ready_thread(fixture._directory.path()).await,
        producer(),
    )
    .await
    .unwrap();

    assert_eq!(
        completed(&fixture.client, "unknown-seal").await.status(),
        reqwest::StatusCode::OK,
        "hosted-call completion must not settle the native producer seal"
    );
    for _ in 0..2 {
        assert!(matches!(
            fixture.finish().await,
            HostedObservation::Observed {
                input_seal: InputSealObservation::Unconfirmed(_),
                seal: SealObservation::Pending,
                resident: ResidentObservation::Pending,
                http: HttpObservation::Pending,
            }
        ));
    }
    assert_eq!(backend.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(
        fixture.slot.lock().is_some(),
        "unknown cleanup remains retained"
    );
    assert!(campaign.actor.terminal().get().is_none());
    fixture.dispose_http().await;
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn lost_input_seal_waiter_reuses_the_retained_operation() {
    let campaign = test_campaign::TestCampaign::start().await;
    let fixture = HttpFixture::canonical_requiring_input_seal(campaign.actor.clone()).await;
    let input_seal_entered = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let backend = SealBackend::held(input_seal_entered.clone(), release.clone());
    begin_input_seal(
        &fixture.owner,
        backend.clone(),
        queue_ready_thread(fixture._directory.path()).await,
        producer(),
    )
    .await
    .unwrap();
    let owner = fixture.owner.clone();
    let waiter = tokio::spawn(async move {
        observe(&owner, CompletionBoundary::AwaitingNativeDecision, limit()).await
    });
    entered(&input_seal_entered).await;
    waiter.abort();
    let _ = waiter.await;
    assert_eq!(backend.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(matches!(
        observe(
            &fixture.owner,
            CompletionBoundary::AwaitingNativeDecision,
            Duration::ZERO
        )
        .await,
        HostedObservation::Pending
    ));
    release.add_permits(1);
    assert!(matches!(
        observe(
            &fixture.owner,
            CompletionBoundary::AwaitingNativeDecision,
            limit()
        )
        .await,
        HostedObservation::Observed {
            input_seal: InputSealObservation::Sealed,
            seal: SealObservation::Confirmed(_),
            ..
        }
    ));
    assert_eq!(backend.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    campaign
        .actor
        .shutdown_with_cleanup(cancelled())
        .await
        .unwrap();
    confirmed_http(fixture.finish().await, campaign.actor.identity());
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn hosted_live_seal_keeps_completion_until_abort_and_drains_exact_actor() {
    let campaign = test_campaign::TestCampaign::start().await;
    let actor = campaign.actor.identity();
    let fixture = HttpFixture::canonical(campaign.actor.clone()).await;
    let result = call(&fixture.client, "let hostedAnswer = 42 :: Int", "initial").await;
    assert_eq!(result["success"], true, "{result:?}");
    let observation = observe(
        &fixture.owner,
        CompletionBoundary::AwaitingNativeDecision,
        limit(),
    )
    .await;
    assert!(
        matches!(observation, HostedObservation::Observed { seal: SealObservation::Confirmed(ref seal), http: HttpObservation::Pending, .. } if seal.actor() == actor),
        "{observation:?}"
    );
    assert_eq!(
        completed(&fixture.client, "initial").await.status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        call(&fixture.client, "hostedAnswer", "late").await["success"],
        false
    );
    assert!(!service_finished(&fixture.owner));
    confirmed_http(fixture.finish().await, actor);
    confirmed_http(fixture.finish().await, actor);
    assert_eq!(campaign.actor.terminal().cleanup().unwrap().actor(), actor);
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
    let weak = Arc::downgrade(&fixture.owner);
    drop(fixture);
    assert!(
        weak.upgrade().is_none(),
        "completed service retains no owner cycle"
    );
}

#[tokio::test]
async fn hosted_terminal_path_uses_retained_cleanup_without_seal() {
    let campaign = test_campaign::TestCampaign::start().await;
    let cleanup = campaign
        .actor
        .shutdown_with_cleanup(cancelled())
        .await
        .unwrap()
        .cleanup;
    assert!(cleanup.is_confirmed(), "{cleanup:?}");
    let fixture = HttpFixture::canonical(campaign.actor.clone()).await;
    let observation = fixture.finish().await;
    assert!(matches!(
        observation,
        HostedObservation::Observed {
            seal: SealObservation::TerminalPath,
            ..
        }
    ));
    confirmed_http(observation, campaign.actor.identity());
    assert!(fixture.owner.lock().await.seal.is_none());
    assert_eq!(
        stop_retired_tool_service(campaign.actor.identity(), &mut fixture.owner.clone()).await,
        CleanupComponentOutcome::Completed,
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn hosted_lost_seal_and_http_waiters_reuse_stored_operations() {
    let campaign = test_campaign::TestCampaign::start().await;
    let endpoint = held(campaign.root_installation.policy.clone(), true, true);
    let fixture = HttpFixture::start(campaign.actor.clone(), endpoint.clone()).await;
    assert_eq!(
        call(&fixture.client, "let retainedAnswer = 7 :: Int", "held").await["success"],
        true
    );
    let owner = fixture.owner.clone();
    let waiting = tokio::spawn(async move {
        observe(&owner, CompletionBoundary::AwaitingNativeDecision, limit()).await
    });
    entered(&endpoint.seal_entered).await;
    waiting.abort();
    let _ = waiting.await;
    assert!(matches!(
        observe(
            &fixture.owner,
            CompletionBoundary::AwaitingNativeDecision,
            Duration::ZERO
        )
        .await,
        HostedObservation::Pending
    ));
    assert_eq!(endpoint.seals.load(std::sync::atomic::Ordering::SeqCst), 1);
    endpoint.seal_release.add_permits(1);
    assert!(matches!(
        observe(
            &fixture.owner,
            CompletionBoundary::AwaitingNativeDecision,
            limit()
        )
        .await,
        HostedObservation::Observed {
            seal: SealObservation::Confirmed(_),
            ..
        }
    ));
    let client = fixture.client.clone();
    let completion = tokio::spawn(async move { completed(&client, "held").await });
    entered(&endpoint.completion_entered).await;
    let task_id = fixture.owner.lock().await.service.as_ref().unwrap().id();
    let owner = fixture.owner.clone();
    let waiting = tokio::spawn(async move {
        observe(&owner, CompletionBoundary::AbortForShutdown, limit()).await
    });
    // Exact terminal is the actor-owned barrier; the held HTTP callback still
    // prevents the original service task from joining after graceful drain.
    tokio::time::timeout(limit(), campaign.actor.terminal().wait())
        .await
        .unwrap();
    waiting.abort();
    let _ = waiting.await;
    assert!(matches!(
        observe(
            &fixture.owner,
            CompletionBoundary::AbortForShutdown,
            Duration::ZERO
        )
        .await,
        HostedObservation::Pending
    ));
    assert_eq!(
        fixture.owner.lock().await.service.as_ref().unwrap().id(),
        task_id
    );
    endpoint.completion_release.add_permits(1);
    assert_eq!(completion.await.unwrap().status(), reqwest::StatusCode::OK);
    confirmed_http(fixture.finish().await, campaign.actor.identity());
    assert_eq!(endpoint.seals.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(
        fixture.slot.lock().is_some(),
        "owner remains anchored after join"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

struct UnsupportedSeal(Arc<dyn ResidentToolEndpoint>);
impl ResidentToolEndpoint for UnsupportedSeal {
    fn tools(&self) -> &[HostedTool] {
        self.0.tools()
    }
    fn instructions(&self) -> Option<&str> {
        self.0.instructions()
    }
    fn dispatch_boxed(&self, invocation: ToolInvocation) -> ResidentToolFuture {
        self.0.dispatch_boxed(invocation)
    }
    fn complete_boxed(
        &self,
        boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> ResidentToolFuture {
        self.0.complete_boxed(boundary)
    }
    // Exercise the owning trait's genuine unsupported seal default.
}

#[tokio::test]
async fn hosted_unsupported_seal_retains_live_actor_and_http() {
    let campaign = test_campaign::TestCampaign::start().await;
    let fixture = HttpFixture::start(
        campaign.actor.clone(),
        Arc::new(UnsupportedSeal(campaign.root_installation.policy.clone())),
    )
    .await;
    assert_eq!(
        call(
            &fixture.client,
            "let unsupportedAnswer = 5 :: Int",
            "unsupported"
        )
        .await["success"],
        true
    );
    let observation = fixture.finish().await;
    assert!(
        matches!(
            observation,
            HostedObservation::Observed {
                seal: SealObservation::Failed(_),
                resident: ResidentObservation::Pending,
                http: HttpObservation::Pending,
                ..
            }
        ),
        "{observation:?}"
    );
    assert!(campaign.actor.terminal().get().is_none());
    assert!(!service_finished(&fixture.owner));
    assert_eq!(
        completed(&fixture.client, "unsupported").await.status(),
        reqwest::StatusCode::OK
    );
    campaign
        .actor
        .shutdown_with_cleanup(cancelled())
        .await
        .unwrap();
    assert!(matches!(
        fixture.finish().await,
        HostedObservation::Observed {
            seal: SealObservation::Failed(_),
            http: HttpObservation::Pending,
            ..
        }
    ));
    fixture.dispose_http().await;
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn hosted_foreign_real_seal_is_rejected_without_shutdown_or_http_drain() {
    foreign_seal_terminal_race(false).await;
}

#[tokio::test]
async fn hosted_pending_foreign_seal_survives_waiter_loss_and_expected_terminal() {
    foreign_seal_terminal_race(true).await;
}

async fn foreign_seal_terminal_race(terminal_while_pending: bool) {
    let campaign = test_campaign::TestCampaign::start().await;
    let sibling_campaign = test_campaign::TestCampaign::start().await;
    let sibling = sibling_campaign.actor.clone();
    let sibling_policy = sibling_campaign.root_installation.policy.clone();
    let endpoint = held(sibling_policy, terminal_while_pending, false);
    let fixture = HttpFixture::start(campaign.actor.clone(), endpoint.clone()).await;
    assert_eq!(
        call(&fixture.client, "let siblingAnswer = 9 :: Int", "sibling").await["success"],
        true
    );
    if terminal_while_pending {
        let owner = fixture.owner.clone();
        let waiting = tokio::spawn(async move {
            observe(&owner, CompletionBoundary::AbortForShutdown, limit()).await
        });
        entered(&endpoint.seal_entered).await;
        waiting.abort();
        let _ = waiting.await;
        campaign
            .actor
            .shutdown_with_cleanup(cancelled())
            .await
            .unwrap();
        endpoint.seal_release.add_permits(1);
    }
    let observation = fixture.finish().await;
    assert!(
        matches!(
            observation,
            HostedObservation::Observed {
                seal: SealObservation::Failed(_),
                resident: ResidentObservation::Pending,
                http: HttpObservation::Pending,
                ..
            }
        ),
        "{observation:?}"
    );
    assert_eq!(
        campaign.actor.terminal().get().is_some(),
        terminal_while_pending
    );
    assert!(sibling.terminal().get().is_none());
    assert!(!service_finished(&fixture.owner));
    assert_eq!(
        completed(&fixture.client, "sibling").await.status(),
        reqwest::StatusCode::OK
    );
    // A terminal for the expected actor cannot repair a rejected foreign seal.
    if !terminal_while_pending {
        campaign
            .actor
            .shutdown_with_cleanup(cancelled())
            .await
            .unwrap();
    }
    assert_eq!(endpoint.seals.load(std::sync::atomic::Ordering::SeqCst), 1);
    let after_expected_terminal = fixture.finish().await;
    assert!(
        matches!(
            after_expected_terminal,
            HostedObservation::Observed {
                seal: SealObservation::Failed(_),
                resident: ResidentObservation::Pending,
                http: HttpObservation::Pending,
                ..
            }
        ),
        "{after_expected_terminal:?}"
    );
    assert!(sibling.terminal().get().is_none());
    assert!(!service_finished(&fixture.owner));
    assert_eq!(
        completed(&fixture.client, "sibling").await.status(),
        reqwest::StatusCode::OK
    );
    let sibling_cleanup = sibling
        .shutdown_with_cleanup(cancelled())
        .await
        .unwrap()
        .cleanup;
    assert!(
        matches!(account(campaign.actor.identity(), Some(sibling_cleanup)),
        ResidentObservation::Foreign(actor) if actor == sibling.identity())
    );
    // Both actors are now cleaned, but the original failed barrier is retained.
    assert!(matches!(
        fixture.finish().await,
        HostedObservation::Observed {
            seal: SealObservation::Failed(_),
            http: HttpObservation::Pending,
            ..
        }
    ));
    fixture.dispose_http().await;
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
    sibling_campaign.forest.shutdown().await;
    sibling_campaign.hosted.await.unwrap();
}

struct NoFixtureHandlers;
impl tidepool_effect::dispatch::DispatchEffect<CapturedOutput> for NoFixtureHandlers {
    fn dispatch(
        &mut self,
        _: &tidepool_eval::Value,
        _: &tidepool_effect::dispatch::EffectContext<'_, CapturedOutput>,
    ) -> Result<Option<tidepool_effect::Response>, tidepool_effect::error::EffectError> {
        Ok(None)
    }
}

#[tokio::test]
async fn hosted_authored_failed_child_cleanup_retains_http_uncertainty() {
    tidepool_testing::eval_harness::require_extract();
    let declarations = [
        tidepool_mcp::agent_tools_decl(),
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::fs_read_decl(),
    ];
    let effects = tidepool_mcp::ensure_effects_module(&declarations).unwrap();
    let mut include = effects.include_paths().to_vec();
    include.push(tidepool_testing::eval_harness::prelude_path());
    let preamble = insert_preamble_imports(
        &tidepool_mcp::build_preamble(&declarations, false),
        "Tidepool.Agent.Contract",
    );
    let preamble = format!(
        "{preamble}
{}",
        include_str!("failed_child_preamble.hs")
    );
    let templates = resident_workbench_templates(&preamble, "ActorEffects", "");
    let roots: Vec<_> = include.iter().map(PathBuf::as_path).collect();
    let directory = tempfile::tempdir().unwrap();
    let compiled = match run_turn(HaskellTurnRequest {
        turn_text: include_str!("failed_child.hs"),
        templates: &templates,
        include: &roots,
        session_root: directory.path(),
        inject_modules: &[],
        gen: 1,
        verdict: None,
        target: None,
    })
    .unwrap()
    {
        TurnResult::Expr { compiled, .. } => compiled,
        other => panic!("authored policy: {other:?}"),
    };
    let session = fresh_session_id();
    let library = SessionLib::open(session, directory.path(), ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(include.clone());
    let mut machine = ResidentSession::bootstrap(
        &compiled.expr,
        compiled.table.clone(),
        NoFixtureHandlers,
        CapturedOutput::default(),
        include.clone(),
        DEFAULT_NURSERY_SIZE,
        Some(library),
    )
    .unwrap();
    machine.set_effect_execution(
        EffectRunPolicy::SuspendAll,
        LivePayloadPolicy::HASKELL_EFFECT_VALUE,
    );
    let outcome = machine
        .run_with_sites(
            "failed_child_host",
            &compiled.expr,
            &compiled.table,
            &compiled.asks,
        )
        .unwrap();
    let descriptor = tidepool_actor::ActorDescriptor::new(
        "host-failed-child",
        tidepool_actor::ActorPlacement {
            session,
            resource_scope: tidepool_codegen::suspension::RealmId::fresh(),
            lexical_scope: tidepool_codegen::scope::ScopeId::ROOT,
        },
    );
    let (forest, mut events) = ResidentForest::new(
        ActorWorkbenchSource::new(preamble, include),
        session,
        machine,
        None,
        tidepool_actor::Incarnation::FIRST,
    );
    let (actor, task) = forest.admit_root(descriptor, outcome).await.unwrap();
    let Some(LocalResidentDeployment::PolicyInstalled(installation)) = events.recv().await else {
        panic!("real resident policy absent");
    };
    let fixture = HttpFixture::start(actor.clone(), installation.policy).await;
    let response: serde_json::Value = fixture
        .client
        .post(format!("{URL}/call"))
        .json(&serde_json::json!({
            "protocolVersion":protocol(), "threadId":THREAD, "turnId":"spawn",
            "callId":"spawn", "contextCallId":"spawn", "namespace":"tidepool_actor",
            "tool":"spawn_child", "arguments":{"seed":8},
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["success"], true, "{response:?}");
    assert_eq!(
        completed(&fixture.client, "spawn").await.status(),
        reqwest::StatusCode::OK
    );
    let observation = fixture.finish().await;
    let HostedObservation::Observed {
        resident: ResidentObservation::Accounted(cleanup),
        http: HttpObservation::Pending,
        ..
    } = observation
    else {
        panic!("{observation:?}")
    };
    assert_eq!(cleanup.actor(), actor.identity());
    assert!(!cleanup.is_confirmed(), "{cleanup:?}");
    assert!(
        matches!(
            cleanup.children(),
            tidepool_actor::CleanupComponentOutcome::Unconfirmed(_)
        ),
        "{cleanup:?}"
    );
    assert!(!service_finished(&fixture.owner));
    // Fixture teardown, deliberately not claimed as consumer cleanup evidence.
    // The consumer correctly leaves HTTP pending on the failed child domain.
    fixture.dispose_http().await;
    forest.shutdown().await;
    task.await.unwrap();
}

struct ShutdownGate {
    entered: Semaphore,
    release: Semaphore,
    calls: std::sync::atomic::AtomicUsize,
}
struct GatedBehavior(Arc<ShutdownGate>);
impl tidepool_actor::KernelBehavior for GatedBehavior {
    fn start<'a>(
        &'a mut self,
        _: &'a tidepool_actor::KernelContext,
    ) -> BoxFuture<'a, Result<tidepool_actor::KernelStep<()>, tidepool_actor::KernelBehaviorError>>
    {
        Box::pin(async { Ok(tidepool_actor::KernelStep::Continue(())) })
    }
    fn cast<'a>(
        &'a mut self,
        _: &'a tidepool_actor::KernelContext,
        _: ActorRef,
        _: tidepool_actor::MailboxValue,
    ) -> BoxFuture<'a, Result<tidepool_actor::KernelStep<()>, tidepool_actor::KernelBehaviorError>>
    {
        Box::pin(async { panic!("unexpected cast") })
    }
    fn call<'a>(
        &'a mut self,
        _: &'a tidepool_actor::KernelContext,
        _: ActorRef,
        _: tidepool_actor::CallAncestry,
        _: tidepool_actor::MailboxValue,
    ) -> BoxFuture<
        'a,
        Result<
            tidepool_actor::KernelStep<tidepool_actor::MailboxValue>,
            tidepool_actor::KernelBehaviorError,
        >,
    > {
        Box::pin(async { panic!("unexpected call") })
    }
    fn tool<'a>(
        &'a mut self,
        _: &'a tidepool_actor::KernelContext,
        _: ToolInvocation,
    ) -> BoxFuture<
        'a,
        Result<
            tidepool_actor::KernelStep<serde_json::Value>,
            tidepool_actor::KernelInvocationFailure,
        >,
    > {
        Box::pin(async { panic!("unexpected tool") })
    }
    fn workbench<'a>(
        &'a mut self,
        _: &'a tidepool_actor::KernelContext,
        _: tidepool_runtime::session::WorkbenchRequest,
        _: Option<Arc<tidepool_actor::WorkbenchExecutionControl>>,
    ) -> BoxFuture<
        'a,
        Result<
            tidepool_actor::KernelStep<tidepool_runtime::session::WorkbenchResponse>,
            tidepool_actor::KernelInvocationFailure,
        >,
    > {
        Box::pin(async { panic!("unexpected workbench") })
    }
    fn external_application_failed<'a>(
        &'a mut self,
        _: &'a tidepool_actor::KernelContext,
        _: tidepool_actor::ExternalApplicationFailure,
    ) -> BoxFuture<'a, tidepool_actor::ExternalFailureDisposition> {
        Box::pin(async { panic!("unexpected external application") })
    }
    fn shutdown<'a>(
        &'a mut self,
        _: &'a tidepool_actor::KernelContext,
        _: &'a ActorTerminal,
    ) -> BoxFuture<'a, Result<(), tidepool_actor::KernelBehaviorError>> {
        Box::pin(async move {
            self.0
                .calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.0.entered.add_permits(1);
            self.0.release.acquire().await.unwrap().forget();
            Ok(())
        })
    }
    fn stopped<'a>(
        &'a mut self,
        _: &'a tidepool_actor::KernelContext,
        _: &'a ActorTerminal,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
    fn child_exited(&mut self, _: tidepool_actor::ChildExitNotice) -> BoxFuture<'_, ()> {
        Box::pin(async { panic!("unexpected child") })
    }
}

struct ActorSealEndpoint(LocalActorRef, Vec<HostedTool>);
impl ResidentToolEndpoint for ActorSealEndpoint {
    fn tools(&self) -> &[HostedTool] {
        &self.1
    }
    fn instructions(&self) -> Option<&str> {
        None
    }
    fn dispatch_boxed(&self, _: ToolInvocation) -> ResidentToolFuture {
        Box::pin(async { Err(ResidentToolError::Unavailable("no fixture tools".into())) })
    }
    fn seal_hosted_work_boxed(
        &self,
    ) -> BoxFuture<'static, Result<HostedWorkSeal, ResidentToolError>> {
        let actor = self.0.clone();
        Box::pin(async move { actor.seal_hosted_work().await.map_err(Into::into) })
    }
}

#[tokio::test]
async fn hosted_lost_pending_shutdown_waiter_retains_real_operation() {
    let gate = Arc::new(ShutdownGate {
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let (actor, task) = tidepool_actor::spawn_local_actor(None, GatedBehavior(gate.clone()))
        .await
        .unwrap();
    let fixture = HttpFixture::start(
        actor.clone(),
        Arc::new(ActorSealEndpoint(
            actor.clone(),
            vec![HostedTool::Custom(tidepool_tool::CustomToolDeclaration {
                name: "unused".into(),
                description: "No invocations supported by shutdown fixture".into(),
            })],
        )),
    )
    .await;
    assert!(
        matches!(observe(&fixture.owner, CompletionBoundary::AwaitingNativeDecision, limit()).await,
        HostedObservation::Observed { seal: SealObservation::Confirmed(seal), http: HttpObservation::Pending, .. } if seal.actor() == actor.identity())
    );
    let owner = fixture.owner.clone();
    let waiter = tokio::spawn(async move {
        observe(&owner, CompletionBoundary::AbortForShutdown, limit()).await
    });
    entered(&gate.entered).await;
    assert!(actor.terminal().get().is_none(), "hook is still running");
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert!(matches!(
        fixture.owner.lock().await.shutdown,
        Some(Operation::Pending(_))
    ));
    assert!(matches!(
        observe(
            &fixture.owner,
            CompletionBoundary::AbortForShutdown,
            Duration::ZERO
        )
        .await,
        HostedObservation::Pending
    ));
    assert!(matches!(
        fixture.owner.lock().await.shutdown,
        Some(Operation::Pending(_))
    ));
    assert_eq!(gate.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(actor.terminal().cleanup().is_none());
    gate.release.add_permits(1);
    let observation = fixture.finish().await;
    let HostedObservation::Observed {
        resident: ResidentObservation::Accounted(cleanup),
        http: HttpObservation::Pending,
        ..
    } = observation
    else {
        panic!("{observation:?}")
    };
    assert_eq!(cleanup.actor(), actor.identity());
    assert!(matches!(
        cleanup.hook(),
        tidepool_actor::CleanupComponentOutcome::Confirmed
    ));
    assert!(matches!(
        cleanup.realm(),
        tidepool_actor::CleanupComponentOutcome::Unsupported
    ));
    assert!(!cleanup.is_confirmed());
    assert!(matches!(
        fixture.owner.lock().await.shutdown,
        Some(Operation::Finished(Ok(_)))
    ));
    assert_eq!(gate.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(!service_finished(&fixture.owner));
    fixture.dispose_http().await;
    tokio::time::timeout(limit(), task).await.unwrap().unwrap();
    let weak = Arc::downgrade(&fixture.owner);
    drop(fixture);
    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn hosted_initially_terminal_actor_cannot_drain_foreign_endpoint() {
    let campaign = test_campaign::TestCampaign::start().await;
    let sibling = test_campaign::TestCampaign::start().await;
    campaign
        .actor
        .shutdown_with_cleanup(cancelled())
        .await
        .unwrap();
    let fixture = HttpFixture::start(
        campaign.actor.clone(),
        sibling.root_installation.policy.clone(),
    )
    .await;
    let observation = fixture.finish().await;
    let rejected = matches!(
        observation,
        HostedObservation::Observed {
            seal: SealObservation::Failed(_),
            resident: ResidentObservation::Pending,
            http: HttpObservation::Pending,
            ..
        }
    );
    let sibling_live = sibling.actor.terminal().get().is_none();
    let service_was_finished = service_finished(&fixture.owner);
    sibling
        .actor
        .shutdown_with_cleanup(cancelled())
        .await
        .unwrap();
    fixture.dispose_http().await;
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
    sibling.forest.shutdown().await;
    sibling.hosted.await.unwrap();
    assert!(
        !service_was_finished,
        "foreign service finished before cleanup"
    );
    assert!(sibling_live);
    assert!(
        rejected,
        "foreign endpoint used unrelated terminal: {observation:?}"
    );
}
