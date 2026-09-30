use super::embedded_service::{
    attach_actor, attach_checkpoint_actor, drive_conversation_with_transport, EmbeddedService,
};
use super::test_campaign::TestCampaign;
use async_trait::async_trait;
use exomonad_actor::LocalResidentDeployment;
use harness::{
    engine::ResponsesTransport,
    model::{AgentPath, CallId, ConversationIdentity, Effort},
    transport::{Auth, ResponsesRequest, ResponsesTurn, TransportError, Usage},
    turn::JobOutput,
};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{mpsc, watch, Notify};

fn log_phase(phase: &str) {
    use std::io::Write;

    let mut stderr = std::io::stderr().lock();
    writeln!(stderr, "embedded checkpoint children: {phase}").unwrap();
    stderr.flush().unwrap();
}

struct Offline;

impl Auth for Offline {
    fn access(&self) -> Result<(String, String), TransportError> {
        panic!("the scripted embedded checkpoint transport must not request credentials")
    }
}

#[derive(Clone)]
struct ParentTransport {
    round: Arc<AtomicUsize>,
    children_used: Arc<Notify>,
    setup_requested: Arc<Notify>,
    setup_ready: Arc<Notify>,
    capture_failure_observed: Arc<Notify>,
    admission_requested: Arc<Notify>,
}

#[derive(Clone)]
struct ScopeSetupTransport {
    round: Arc<AtomicUsize>,
    setup_requested: Arc<Notify>,
    result_presented: Arc<Notify>,
}

#[async_trait]
impl ResponsesTransport for ScopeSetupTransport {
    async fn create(&self, request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        let round = self.round.fetch_add(1, Ordering::SeqCst) + 1;
        let items = if round == 1 {
            self.setup_requested.notify_one();
            vec![harness::item::Item(json!({
                "type":"custom_tool_call", "call_id":"checkpoint-scope-setup",
                "name":"haskell",
                "input":include_str!("embedded_checkpoint_scope_setup.hs")
            }))]
        } else {
            // A final answer while the cell is pending makes the Engine wait
            // for settlement and request another turn with its durable output.
            if let Some(output) = request.input.iter().find(|item| {
                item.0["type"] == "custom_tool_call_output"
                    && item.0["call_id"] == "checkpoint-scope-setup"
            }) {
                let receipt = serde_json::from_str(output.0["output"].as_str().unwrap()).unwrap();
                assert_committed_haskell_value(&receipt, "True");
                self.result_presented.notify_one();
            }
            vec![harness::item::Item(json!({
                "type":"message", "role":"assistant", "phase":"final_answer",
                "content":[{"type":"output_text","text":"scope setup finished"}]
            }))]
        };
        Ok(ResponsesTurn {
            response_id: format!("checkpoint-setup-only-{round}"),
            items,
            usage: Usage::default(),
        })
    }
}

#[async_trait]
impl ResponsesTransport for ParentTransport {
    async fn create(&self, _request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        let round = self.round.fetch_add(1, Ordering::SeqCst) + 1;
        let items = match round {
            1 => vec![harness::item::Item(json!({
                "type":"custom_tool_call", "call_id":"checkpoint-scope-setup",
                "name":"haskell",
                "input":include_str!("embedded_checkpoint_scope_setup.hs")
            }))],
            2 => {
                self.setup_requested.notify_one();
                self.setup_ready.notified().await;
                vec![harness::item::Item(json!({
                    "type":"custom_tool_call", "call_id":"checkpoint-parent-capture-failure",
                    "name":"haskell",
                    "input":include_str!("embedded_checkpoint_capture_and_children.hs")
                }))]
            }
            3 => {
                self.capture_failure_observed.notified().await;
                self.admission_requested.notify_one();
                vec![harness::item::Item(json!({
                    "type":"custom_tool_call", "call_id":"checkpoint-parent-admit-stored-children",
                    "name":"haskell",
                    "input":include_str!("embedded_checkpoint_admit_stored_children.hs")
                }))]
            }
            4 => {
                self.children_used.notified().await;
                vec![harness::item::Item(json!({
                    "type":"message", "role":"assistant", "phase":"final_answer",
                    "content":[{"type":"output_text","text":"checkpoint children finished"}]
                }))]
            }
            other => panic!("unexpected parent model request {other}"),
        };
        Ok(ResponsesTurn {
            response_id: format!("checkpoint-parent-{round}"),
            items,
            usage: Usage::default(),
        })
    }
}

#[derive(Clone)]
struct ChildTransport {
    label: &'static str,
    round: Arc<AtomicUsize>,
    uses: mpsc::UnboundedSender<(&'static str, usize)>,
    runtime: Arc<super::embedded_harness::EmbeddedHarnessRuntime>,
    conversation: Arc<harness::embedding::Conversation>,
}

#[async_trait]
impl ResponsesTransport for ChildTransport {
    async fn create(&self, _request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        let round = self.round.fetch_add(1, Ordering::SeqCst) + 1;
        let items = match round {
            1 => vec![harness::item::Item(json!({
                "type":"custom_tool_call",
                "call_id":format!("checkpoint-child-{}-after-capture-failure", self.label),
                "name":"haskell", "input":"(x, getX)"
            }))],
            2 => {
                let receipt = operation_with_timeout(
                    &self.runtime,
                    &self.conversation,
                    &format!("checkpoint-child-{}-after-capture-failure", self.label),
                    Duration::from_secs(60),
                )
                .await
                .unwrap();
                assert_committed_haskell_value(&receipt, "(41, 42)");
                self.uses.send((self.label, 1)).unwrap();
                vec![harness::item::Item(json!({
                    "type":"message", "role":"assistant", "phase":"final_answer",
                    "content":[{"type":"output_text","text":"I read the captured Haskell context."}]
                }))]
            }
            3 => vec![harness::item::Item(json!({
                "type":"custom_tool_call",
                "call_id":format!("checkpoint-child-{}-after-parent-failure", self.label),
                "name":"haskell", "input":"(x, getX)"
            }))],
            4 => {
                let receipt = operation_with_timeout(
                    &self.runtime,
                    &self.conversation,
                    &format!("checkpoint-child-{}-after-parent-failure", self.label),
                    Duration::from_secs(60),
                )
                .await
                .unwrap();
                assert_committed_haskell_value(&receipt, "(41, 42)");
                self.uses.send((self.label, 2)).unwrap();
                vec![harness::item::Item(json!({
                    "type":"message", "role":"assistant", "phase":"final_answer",
                    "content":[{"type":"output_text","text":"The retained context remains usable."}]
                }))]
            }
            other => panic!("unexpected child model request {other}"),
        };
        Ok(ResponsesTurn {
            response_id: format!("checkpoint-child-{}-{round}", self.label),
            items,
            usage: Usage::default(),
        })
    }
}

fn assert_committed_haskell_value(response: &Value, expected: &str) {
    let committed_run =
        serde_json::to_value(tidepool_runtime::session::WorkbenchRunStatus::Committed).unwrap();
    let committed_item =
        serde_json::to_value(tidepool_runtime::session::WorkbenchItemStatus::Committed).unwrap();
    assert_eq!(response["status"], committed_run, "{response}");
    let item = response["items"]
        .as_array()
        .expect("WorkbenchResponse.items must be an array")
        .last()
        .expect("Haskell operation must have a result item");
    assert_eq!(item["status"], committed_item, "{response}");
    assert_eq!(item["output"], expected, "{response}");
}

fn embedded_launch_config(files: &tempfile::TempDir) -> crate::exomonad::EmbeddedLaunchConfig {
    let assets = files.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
    let secret_file = files.path().join("session-secret");
    std::fs::write(&secret_file, "embedded-children-secret-is-long-enough").unwrap();
    let auth_file = files.path().join("codex-auth.json");
    std::fs::write(&auth_file, "{}").unwrap();
    crate::exomonad::EmbeddedLaunchConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        asset_root: assets,
        session_secret_file: secret_file,
        codex_auth_file: auth_file,
        context_capacity_tokens: 200_000,
        concurrent_jobs: 3,
    }
}

async fn embedded_operation(
    service: &EmbeddedService,
    conversation: &harness::embedding::Conversation,
    call_id: &str,
) -> Value {
    embedded_operation_with_timeout(service, conversation, call_id, Duration::from_secs(30))
        .await
        .unwrap_or_else(|error| panic!("embedded Haskell operation {call_id} failed: {error}"))
}

async fn embedded_operation_with_timeout(
    service: &EmbeddedService,
    conversation: &harness::embedding::Conversation,
    call_id: &str,
    timeout: Duration,
) -> Result<Value, String> {
    operation_with_timeout(&service.runtime, conversation, call_id, timeout).await
}

async fn operation_with_timeout(
    runtime: &super::embedded_harness::EmbeddedHarnessRuntime,
    conversation: &harness::embedding::Conversation,
    call_id: &str,
    timeout: Duration,
) -> Result<Value, String> {
    let origin = ConversationIdentity::Embedded {
        run: conversation.identity().run.clone(),
        actor: conversation.identity().actor.clone(),
        incarnation: conversation.identity().incarnation.clone(),
    };
    let call = CallId(call_id.to_owned());
    let deadline = tokio::time::Instant::now() + timeout;
    let claim = loop {
        if let Some(claim) = runtime
            .store()
            .claims(&call)
            .unwrap()
            .into_iter()
            .find(|claim| claim.operation.origin == origin)
        {
            break claim;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "embedded Haskell operation {call_id} was not admitted within {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    match tokio::time::timeout_at(deadline, runtime.scheduler().wait(&claim.operation))
        .await
        .unwrap_or_else(|_| panic!("embedded Haskell operation {call_id} did not settle"))
        .unwrap()
    {
        JobOutput::Completed(result) => result,
        other => panic!("embedded Haskell operation {call_id} failed: {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn embedded_checkpoint_scope_setup_starts_its_haskell_actor() {
    let campaign = TestCampaign::start().await;
    let actor = campaign.actor.identity();
    let files = tempfile::tempdir().unwrap();
    let settings = embedded_launch_config(&files);
    let mut service = EmbeddedService::prepare(campaign.session_root.path(), &settings)
        .await
        .unwrap();
    let root = attach_actor(
        &service,
        campaign.session_root.path(),
        AgentPath("/root".into()),
        None,
        campaign.root_installation.clone(),
        Some("start checkpoint setup-only fixture".into()),
    )
    .await
    .unwrap();
    let root_conversation = Arc::clone(&root.conversation);
    let setup_requested = Arc::new(Notify::new());
    let result_presented = Arc::new(Notify::new());
    let cancellation = root.cancellation.clone();
    let transport = ScopeSetupTransport {
        round: Arc::new(AtomicUsize::new(0)),
        setup_requested: Arc::clone(&setup_requested),
        result_presented: Arc::clone(&result_presented),
    };
    let (lifecycle, mut lifecycle_rx) =
        watch::channel((Some(actor), harness::server::HostActorLifecycle::Waiting));
    let runtime = Arc::clone(&service.runtime);
    let root_driver = tokio::spawn(async move {
        drive_conversation_with_transport::<Offline, _>(
            root.driver,
            runtime,
            &settings,
            "offline-checkpoint-setup-only".into(),
            Effort::Medium,
            "embedded checkpoint setup-only fixture".into(),
            root.cancellation_rx,
            lifecycle,
            actor,
            transport,
        )
        .await
    });

    log_phase("setup-only test waiting for fixture turn");
    tokio::time::timeout(Duration::from_secs(60), setup_requested.notified())
        .await
        .expect("root Engine did not request the setup-only fixture turn");
    log_phase("setup-only test waiting for R.start workbench receipt");
    let setup = embedded_operation_with_timeout(
        &service,
        &root_conversation,
        "checkpoint-scope-setup",
        Duration::from_secs(120),
    )
    .await
    .unwrap();
    log_phase(&format!("setup-only workbench receipt: {setup}"));
    assert_committed_haskell_value(&setup, "True");
    tokio::time::timeout(Duration::from_secs(10), result_presented.notified())
        .await
        .expect("setup-only provider did not receive the committed cell output");
    tokio::time::timeout(Duration::from_secs(10), async {
        while lifecycle_rx.borrow_and_update().1 != harness::server::HostActorLifecycle::Waiting {
            lifecycle_rx.changed().await.unwrap();
        }
    })
    .await
    .expect("setup-only Engine did not finish its result turn");
    cancellation.send_replace(true);

    let root_result = tokio::time::timeout(Duration::from_secs(10), root_driver)
        .await
        .expect("setup-only root conversation did not finish")
        .unwrap();
    assert!(
        root_result.is_ok(),
        "setup-only root embedded Engine failed: {root_result:?}"
    );
    service.shutdown().await.unwrap();
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn published_embedded_checkpoint_survives_capture_cell_failure() {
    let mut campaign = TestCampaign::start().await;
    let actor = campaign.actor.identity();
    let files = tempfile::tempdir().unwrap();
    let settings = embedded_launch_config(&files);
    let mut service = EmbeddedService::prepare(campaign.session_root.path(), &settings)
        .await
        .unwrap();
    let root = attach_actor(
        &service,
        campaign.session_root.path(),
        AgentPath("/root".into()),
        None,
        campaign.root_installation.clone(),
        Some("start checkpoint fixture".into()),
    )
    .await
    .unwrap();
    let root_conversation = Arc::clone(&root.conversation);
    let parent_transport = ParentTransport {
        round: Arc::new(AtomicUsize::new(0)),
        children_used: Arc::new(Notify::new()),
        setup_requested: Arc::new(Notify::new()),
        setup_ready: Arc::new(Notify::new()),
        capture_failure_observed: Arc::new(Notify::new()),
        admission_requested: Arc::new(Notify::new()),
    };
    let release_parent = Arc::clone(&parent_transport.children_used);
    let setup_requested = Arc::clone(&parent_transport.setup_requested);
    let setup_ready = Arc::clone(&parent_transport.setup_ready);
    let capture_failure_observed = Arc::clone(&parent_transport.capture_failure_observed);
    let admission_requested = Arc::clone(&parent_transport.admission_requested);
    let root_cancellation = root.cancellation.clone();
    let (lifecycle, mut lifecycle_rx) =
        watch::channel((Some(actor), harness::server::HostActorLifecycle::Waiting));
    let settings_for_root = settings.clone();
    let runtime = Arc::clone(&service.runtime);
    let root_driver = tokio::spawn(async move {
        drive_conversation_with_transport::<Offline, _>(
            root.driver,
            runtime,
            &settings_for_root,
            "offline-checkpoint".into(),
            Effort::Medium,
            "embedded checkpoint fixture".into(),
            root.cancellation_rx,
            lifecycle,
            actor,
            parent_transport,
        )
        .await
    });

    log_phase("waiting for parent setup turn");
    tokio::time::timeout(Duration::from_secs(60), setup_requested.notified())
        .await
        .expect("root Engine did not request a turn after lexical scope setup");
    log_phase("waiting for scope setup operation");
    let captured = embedded_operation_with_timeout(
        &service,
        &root_conversation,
        "checkpoint-scope-setup",
        Duration::from_secs(90),
    )
    .await
    .unwrap();
    assert_committed_haskell_value(&captured, "True");
    log_phase("scope setup committed; releasing capture failure turn");
    setup_ready.notify_one();

    // Observe the exact workbench receipt before allowing the parent to submit
    // the later cell that admits children from the retained token.
    log_phase("waiting for capture cell failure receipt");
    let failed_parent = embedded_operation_with_timeout(
        &service,
        &root_conversation,
        "checkpoint-parent-capture-failure",
        Duration::from_secs(90),
    )
    .await
    .expect_err("capture cell must fail during effect execution before child admission");
    assert!(
        failed_parent.contains("expected checkpoint capture execution failure"),
        "capture cell failed for an unexpected reason: {failed_parent}"
    );
    assert!(
        failed_parent.contains("Committed (capture context checkpoint)"),
        "the failed cell must retain its completed capture receipt: {failed_parent}"
    );
    assert!(
        failed_parent.contains("Committed (cast)"),
        "the failed cell must retain delivery of the captured token: {failed_parent}"
    );
    log_phase("capture failure receipt observed; allowing later admission turn");
    capture_failure_observed.notify_one();
    tokio::time::timeout(Duration::from_secs(60), admission_requested.notified())
        .await
        .expect("parent did not request stored-child admission after capture failure was observed");

    log_phase("waiting for stored-child admission operation");
    let admitted_children = embedded_operation_with_timeout(
        &service,
        &root_conversation,
        "checkpoint-parent-admit-stored-children",
        Duration::from_secs(90),
    )
    .await
    .unwrap();
    assert_committed_haskell_value(&admitted_children, "True");
    log_phase("stored-child admission committed; waiting for child deployments");

    let mut children = Vec::with_capacity(2);
    for _ in 0..2 {
        let installation = campaign
            .next_deployment(
                "checkpoint child policy installation",
                Duration::from_secs(120),
                |event| match event {
                    LocalResidentDeployment::PolicyInstalled(installation)
                        if installation.checkpoint.is_some()
                            && installation.context_parent == Some(actor) =>
                    {
                        Ok(*installation)
                    }
                    other => Err(other),
                },
            )
            .await;
        campaign.authority.install_grant(
            installation.actor.identity().into(),
            super::worktree_grant(installation.effective_role.role()),
        );
        installation
            .fork_gate
            .as_ref()
            .expect("checkpoint child has a fork gate")
            .mark_ready()
            .unwrap();
        children.push(installation);
    }

    let call_ids = [
        "checkpoint-child-alpha-after-capture-failure",
        "checkpoint-child-beta-after-capture-failure",
    ];
    let (uses_tx, mut uses_rx) = mpsc::unbounded_channel();
    let mut child_drivers = Vec::with_capacity(2);
    let mut child_conversations = Vec::with_capacity(2);
    let mut child_cancellations = Vec::with_capacity(2);
    for (installation, label) in children.into_iter().zip(["alpha", "beta"]) {
        log_phase(if label == "alpha" {
            "attaching alpha child"
        } else {
            "attaching beta child"
        });
        let child_actor = installation.actor.identity();
        let initial_input = installation.initial_user_message.clone();
        let child = attach_checkpoint_actor(
            &service.runtime,
            campaign.session_root.path(),
            installation,
            initial_input,
        )
        .await
        .unwrap();
        // The production host forwards this exact mounted request after
        // attachment. Policy installation alone supplies no conversation input.
        let activation = campaign
            .next_deployment(
                "checkpoint child mounted request activation",
                Duration::from_secs(120),
                |event| match event {
                    LocalResidentDeployment::SessionReady { activation }
                        if activation.id.actor() == child_actor =>
                    {
                        Ok(activation)
                    }
                    other => Err(other),
                },
            )
            .await;
        child
            .conversation
            .input(
                &format!(
                    "session:{}:{}",
                    activation.request.0,
                    activation.id.sequence()
                ),
                "resident",
                &activation.message,
            )
            .await
            .unwrap();
        child_conversations.push(Arc::clone(&child.conversation));
        child_cancellations.push(child.cancellation.clone());
        let (lifecycle, _lifecycle_rx) = watch::channel((
            Some(child_actor),
            harness::server::HostActorLifecycle::Waiting,
        ));
        let settings_for_child = settings.clone();
        let runtime = Arc::clone(&service.runtime);
        let transport = ChildTransport {
            label,
            round: Arc::new(AtomicUsize::new(0)),
            uses: uses_tx.clone(),
            runtime: Arc::clone(&runtime),
            conversation: Arc::clone(&child.conversation),
        };
        child_drivers.push(tokio::spawn(async move {
            drive_conversation_with_transport::<Offline, _>(
                child.driver,
                runtime,
                &settings_for_child,
                "offline-checkpoint-child".into(),
                Effort::Medium,
                "checkpoint child fixture".into(),
                child.cancellation_rx,
                lifecycle,
                child_actor,
                transport,
            )
            .await
        }));
    }

    let mut first_child_uses = std::collections::HashSet::new();
    for _ in 0..2 {
        log_phase("waiting for a child's first Haskell call");
        let (label, use_index) = tokio::time::timeout(Duration::from_secs(60), uses_rx.recv())
            .await
            .expect("children did not finish their first captured-context Haskell calls")
            .expect("child use observer closed");
        assert_eq!(
            use_index, 1,
            "first child call ordering changed for {label}"
        );
        first_child_uses.insert(label);
    }
    assert_eq!(first_child_uses, ["alpha", "beta"].into_iter().collect());
    log_phase("both children completed their first Haskell calls");

    campaign.assert_no_deployment("released checkpoint must refuse a third child", |event| {
        matches!(event, LocalResidentDeployment::PolicyInstalled(installation)
            if installation.context_parent == Some(actor) && installation.checkpoint.is_some())
    });

    for (conversation, call_id) in child_conversations.iter().zip(call_ids) {
        log_phase("waiting for child call receipt");
        let response = embedded_operation(&service, conversation, call_id).await;
        assert_committed_haskell_value(&response, "(41, 42)");
    }

    // Each child waits for durable input after its first answer. Wake both to
    // make another independent Haskell read from the same captured checkpoint.
    for (conversation, label) in child_conversations.iter().zip(["alpha", "beta"]) {
        conversation
            .input(
                &format!("after-parent-failure-{label}"),
                "operator",
                "Read the captured Haskell context again after the parent failed.",
            )
            .await
            .unwrap();
    }
    let mut later_child_uses = std::collections::HashSet::new();
    for _ in 0..2 {
        log_phase("waiting for a child's post-failure Haskell call");
        let (label, use_index) = tokio::time::timeout(Duration::from_secs(60), uses_rx.recv())
            .await
            .expect("children did not finish post-failure Haskell calls")
            .expect("child use observer closed");
        assert_eq!(
            use_index, 2,
            "post-failure child call ordering changed for {label}"
        );
        later_child_uses.insert(label);
    }
    assert_eq!(later_child_uses, ["alpha", "beta"].into_iter().collect());
    for (conversation, call_id) in child_conversations.iter().zip([
        "checkpoint-child-alpha-after-parent-failure",
        "checkpoint-child-beta-after-parent-failure",
    ]) {
        let response = embedded_operation(&service, conversation, call_id).await;
        assert_committed_haskell_value(&response, "(41, 42)");
    }

    // Let the parent provider finish after both detached child scopes remain
    // usable following the failed capture cell and later release/refusal.
    release_parent.notify_one();
    tokio::time::timeout(Duration::from_secs(10), async {
        while lifecycle_rx.borrow_and_update().1 != harness::server::HostActorLifecycle::Waiting {
            lifecycle_rx.changed().await.unwrap();
        }
    })
    .await
    .expect("root Engine did not retain its final durable head");
    root_cancellation.send_replace(true);

    let root_result = tokio::time::timeout(Duration::from_secs(10), root_driver)
        .await
        .expect("root embedded conversation did not finish")
        .unwrap();
    assert!(
        root_result.is_ok(),
        "root embedded Engine failed: {root_result:?}"
    );
    for cancellation in child_cancellations {
        cancellation.send_replace(true);
    }
    for driver in child_drivers {
        tokio::time::timeout(Duration::from_secs(10), driver)
            .await
            .expect("child embedded Engine did not stop")
            .unwrap()
            .unwrap();
    }
    service.shutdown().await.unwrap();
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
