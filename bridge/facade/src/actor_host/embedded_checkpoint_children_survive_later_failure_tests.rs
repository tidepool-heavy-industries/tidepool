use super::*;
use async_trait::async_trait;
use harness::{
    engine::ResponsesTransport,
    model::{AgentPath, CallId, ConversationIdentity},
    transport::{ResponsesRequest, ResponsesTurn, TransportError, Usage},
    turn::JobOutput,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;

fn log_phase(phase: &str) {
    eprintln!("embedded later-failure children: {phase}");
}

#[derive(Clone)]
struct ParentTransport {
    round: Arc<AtomicUsize>,
    post_failure_used: Arc<Notify>,
    setup_requested: Arc<Notify>,
    setup_ready: Arc<Notify>,
    unfold_requested: Arc<Notify>,
    permit_failure: Arc<Notify>,
    failure_observed: Arc<Notify>,
}

#[async_trait]
impl ResponsesTransport for ParentTransport {
    async fn create(&self, _request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        let round = self.round.fetch_add(1, Ordering::SeqCst) + 1;
        let items = match round {
            1 => vec![harness::item::Item(json!({
                "type":"custom_tool_call", "call_id":"later-failure-scope-setup",
                "name":"haskell",
                "input":tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_later_failure_scope_setup.hs")
            }))],
            2 => {
                self.setup_requested.notify_one();
                self.setup_ready.notified().await;
                vec![harness::item::Item(json!({
                    "type":"custom_tool_call", "call_id":"later-failure-capture-and-unfold",
                    "name":"haskell",
                    "input":tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_later_failure_capture_and_unfold.hs")
                }))]
            }
            3 => {
                self.unfold_requested.notify_one();
                self.permit_failure.notified().await;
                vec![harness::item::Item(json!({
                    "type":"custom_tool_call", "call_id":"later-parent-cell-failure",
                    "name":"haskell",
                    "input":tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_later_failure_parent_error.hs")
                }))]
            }
            4 => {
                self.failure_observed.notify_one();
                self.post_failure_used.notified().await;
                vec![harness::item::Item(json!({
                    "type":"message", "role":"assistant", "phase":"final_answer",
                    "content":[{"type":"output_text","text":"checkpoint children finished"}]
                }))]
            }
            other => panic!("unexpected parent model request {other}"),
        };
        Ok(ResponsesTurn {
            response_id: format!("later-failure-parent-{round}"),
            items,
            usage: Usage::default(),
        })
    }
}

struct ChildScript {
    label: &'static str,
    round: usize,
}

struct HostTransport {
    parent: ParentTransport,
    children: Mutex<HashMap<String, ChildScript>>,
    uses: mpsc::UnboundedSender<(&'static str, usize, ConversationIdentity)>,
    after_failure: watch::Sender<bool>,
}

#[async_trait]
impl ResponsesTransport for HostTransport {
    async fn create(&self, request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        let (prefix, incarnation) = request.session_id.rsplit_once(':').unwrap();
        let (run, path) = prefix.rsplit_once(':').unwrap();
        if path == "/root" {
            return self.parent.create(request).await;
        }
        let actor = AgentPath(path.into());
        let origin = ConversationIdentity::Embedded {
            run: run.into(),
            actor: actor.clone(),
            incarnation: incarnation.into(),
        };
        let (label, round) = {
            let mut children = self.children.lock();
            let label = match children.len() {
                0 => "alpha",
                1 => "beta",
                _ => "unexpected-child",
            };
            let child = children
                .entry(request.session_id)
                .or_insert(ChildScript { label, round: 0 });
            child.round += 1;
            (child.label, child.round)
        };
        log_phase(&format!(
            "child {label} provider request {round}: {origin:?}"
        ));
        let items = match round {
            1 => vec![harness::item::Item(json!({
                "type":"custom_tool_call",
                "call_id":format!("checkpoint-child-{label}-before-parent-failure"),
                "name":"haskell", "input":"(x, getX)"
            }))],
            2 => {
                self.uses.send((label, 1, origin)).unwrap();
                let mut after_failure = self.after_failure.subscribe();
                while !*after_failure.borrow_and_update() {
                    after_failure.changed().await.unwrap();
                }
                vec![harness::item::Item(json!({
                    "type":"custom_tool_call",
                    "call_id":format!("checkpoint-child-{label}-after-parent-failure"),
                    "name":"haskell", "input":"(x, getX)"
                }))]
            }
            3 => {
                self.uses.send((label, 2, origin)).unwrap();
                vec![harness::item::Item(json!({
                    "type":"message", "role":"assistant", "phase":"final_answer",
                    "content":[{"type":"output_text","text":"The retained context remains usable."}]
                }))]
            }
            other => panic!("unexpected child model request {other}"),
        };
        Ok(ResponsesTurn {
            response_id: format!("checkpoint-child-{label}-{round}"),
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

async fn embedded_operation(
    runtime: &embedded_harness::EmbeddedHarnessRuntime,
    origin: &ConversationIdentity,
    call_id: &str,
) -> Result<Value, String> {
    let call = CallId(call_id.to_owned());
    let claim = runtime
        .store()
        .claims(&call)
        .unwrap()
        .into_iter()
        .find(|claim| &claim.operation.origin == origin)
        .unwrap_or_else(|| panic!("embedded Haskell operation {call_id} was not admitted"));
    match tokio::time::timeout(
        Duration::from_secs(90),
        runtime.scheduler().wait(&claim.operation),
    )
    .await
    .unwrap_or_else(|_| panic!("embedded Haskell operation {call_id} did not settle"))
    .unwrap()
    {
        JobOutput::Completed(result) => result.map_err(|error| error.to_string()),
        other => panic!("embedded Haskell operation {call_id} failed: {other:?}"),
    }
}

#[tokio::test]
async fn embedded_checkpoint_children_survive_a_later_parent_cell_failure() {
    let files = tempfile::tempdir().unwrap();
    let assets = files.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
    let secret_file = files.path().join("session-secret");
    std::fs::write(&secret_file, "embedded-children-secret-is-long-enough").unwrap();
    let auth_file = files.path().join("codex-auth.json");
    std::fs::write(&auth_file, "{}").unwrap();
    let settings = crate::exomonad::EmbeddedLaunchConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Https,
        public_origin: None,
        asset_root: assets,
        browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret,
        session_secret_file: Some(secret_file),
        provider: crate::exomonad::EmbeddedModelProvider::Codex,
        credential_file: auth_file,
        context_capacity_tokens: 200_000,
        concurrent_jobs: 3,
    };
    let parent = ParentTransport {
        round: Arc::new(AtomicUsize::new(0)),
        post_failure_used: Arc::new(Notify::new()),
        setup_requested: Arc::new(Notify::new()),
        setup_ready: Arc::new(Notify::new()),
        unfold_requested: Arc::new(Notify::new()),
        permit_failure: Arc::new(Notify::new()),
        failure_observed: Arc::new(Notify::new()),
    };
    let (uses_tx, mut uses_rx) = mpsc::unbounded_channel();
    let transport = Arc::new(HostTransport {
        parent: parent.clone(),
        children: Mutex::new(HashMap::new()),
        uses: uses_tx,
        after_failure: watch::channel(false).0,
    });
    let provider: Arc<dyn ResponsesTransport> = transport.clone();
    let host = hosted_test_context::HostedTestRuntime::start(&settings, &provider)
        .await
        .expect("production checkpoint host starts");
    host.input("Capture child contexts before the later parent failure.")
        .await
        .unwrap();
    let actor = host.context.actor.identity();
    let runtime = Arc::clone(&host.runtime);
    let root_origin = ConversationIdentity::Embedded {
        run: runtime_namespace(&host.context.config.run_directory.path()),
        actor: AgentPath("/root".into()),
        incarnation: actor.incarnation.0.to_string(),
    };

    tokio::time::timeout(Duration::from_secs(60), parent.setup_requested.notified())
        .await
        .expect("root Engine did not request a turn after lexical scope setup");
    let setup = embedded_operation(&runtime, &root_origin, "later-failure-scope-setup")
        .await
        .unwrap();
    assert_committed_haskell_value(&setup, "True");
    log_phase("scope setup committed; offering capture and unfold");
    parent.setup_ready.notify_one();

    tokio::time::timeout(Duration::from_secs(90), parent.unfold_requested.notified())
        .await
        .expect("root Engine did not request a turn after checkpoint child unfold");
    let unfolded = embedded_operation(&runtime, &root_origin, "later-failure-capture-and-unfold")
        .await
        .unwrap();
    assert_committed_haskell_value(&unfolded, "True");
    log_phase("capture and unfold committed; waiting for initial child reads");

    let mut first_child_uses = std::collections::HashSet::new();
    for _ in 0..2 {
        let (label, use_index, origin) =
            tokio::time::timeout(Duration::from_secs(120), uses_rx.recv())
                .await
                .expect("children did not issue their first captured-context Haskell calls")
                .expect("child use observer closed");
        assert_eq!(use_index, 1);
        let response = embedded_operation(
            &runtime,
            &origin,
            &format!("checkpoint-child-{label}-before-parent-failure"),
        )
        .await
        .unwrap();
        assert_committed_haskell_value(&response, "(41, 42)");
        first_child_uses.insert(label);
    }
    assert_eq!(first_child_uses, ["alpha", "beta"].into_iter().collect());
    log_phase("both initial child reads committed; offering parent execution failure");

    // A successor provider request can precede a pending Haskell result. Only
    // settled reads authorize offering the parent its separate failing cell.
    parent.permit_failure.notify_one();
    tokio::time::timeout(Duration::from_secs(60), parent.failure_observed.notified())
        .await
        .expect("root Engine did not request a turn after the later parent failure");
    let failed_parent = embedded_operation(&runtime, &root_origin, "later-parent-cell-failure")
        .await
        .expect_err("the later parent cell must fail during effect execution");
    assert!(
        failed_parent.contains("intentional later parent Haskell execution failure"),
        "parent cell failed for an unexpected reason: {failed_parent}"
    );
    log_phase("parent execution failure settled; allowing later child reads");
    transport.after_failure.send_replace(true);

    let mut later_child_uses = std::collections::HashSet::new();
    for _ in 0..2 {
        let (label, use_index, origin) =
            tokio::time::timeout(Duration::from_secs(120), uses_rx.recv())
                .await
                .expect("children did not issue post-failure Haskell calls")
                .expect("child use observer closed");
        assert_eq!(use_index, 2);
        let response = embedded_operation(
            &runtime,
            &origin,
            &format!("checkpoint-child-{label}-after-parent-failure"),
        )
        .await
        .unwrap();
        assert_committed_haskell_value(&response, "(41, 42)");
        later_child_uses.insert(label);
    }
    assert_eq!(later_child_uses, ["alpha", "beta"].into_iter().collect());
    parent.post_failure_used.notify_one();

    let children = host
        .context
        .observer
        .installations()
        .into_iter()
        .filter(|installation| installation.checkpoint)
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 2);
    for child in children {
        assert_eq!(child.context_parent, Some(actor));
        assert!(
            child.actor.terminal().get().is_none(),
            "captured owner remains live after both reads"
        );
    }
    host.stop()
        .await
        .expect("production checkpoint host acknowledges cleanup");
}
