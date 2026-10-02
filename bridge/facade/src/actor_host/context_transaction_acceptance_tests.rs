use super::m1_host_tests::host_fixture::RunningBrowserHost;
use super::*;
use async_trait::async_trait;
use harness::{
    engine::ResponsesTransport,
    item::Item,
    model::AgentPath,
    transport::{ResponsesRequest, ResponsesTurn, TransportError},
};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

const CELL_TIMEOUT: Duration = Duration::from_secs(300);
const SECRET: &str = "resident-context-acceptance-secret-32-bytes";

struct RequestedRound {
    request: ResponsesRequest,
    reply: oneshot::Sender<ResponsesTurn>,
}

impl RequestedRound {
    fn is_root(&self) -> bool {
        self.request.session_id.contains(":/root:")
    }

    fn cell(self, call_id: &str, source: &str) {
        self.reply
            .send(ResponsesTurn {
                response_id: format!("response-{call_id}"),
                items: vec![Item(json!({
                    "type": "custom_tool_call", "call_id": call_id,
                    "name": "haskell_sync", "input": source,
                }))],
                usage: Default::default(),
            })
            .expect("resident Engine still awaits its scripted reply");
    }

    fn function(self, call_id: &str, name: &str, arguments: Value) {
        self.reply
            .send(ResponsesTurn {
                response_id: format!("response-{call_id}"),
                items: vec![Item(json!({
                    "type": "function_call", "call_id": call_id,
                    "name": name, "arguments": arguments.to_string(),
                }))],
                usage: Default::default(),
            })
            .expect("resident Engine still awaits its scripted reply");
    }

    fn finish(self) {
        self.reply
            .send(ResponsesTurn {
                response_id: uuid::Uuid::new_v4().simple().to_string(),
                items: vec![Item(json!({
                    "type": "message", "role": "assistant", "phase": "final_answer",
                    "content": [{"type": "output_text", "text": "acceptance complete"}],
                }))],
                usage: Default::default(),
            })
            .expect("resident Engine still awaits its scripted reply");
    }
}

struct ScriptedProvider(mpsc::UnboundedSender<RequestedRound>);

#[async_trait]
impl ResponsesTransport for ScriptedProvider {
    async fn create(&self, request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        let (reply, completed) = oneshot::channel();
        self.0
            .send(RequestedRound { request, reply })
            .expect("acceptance observer remains alive");
        Ok(completed
            .await
            .expect("acceptance supplies the provider reply"))
    }
}

async fn next_round(rounds: &mut mpsc::UnboundedReceiver<RequestedRound>) -> RequestedRound {
    tokio::time::timeout(CELL_TIMEOUT, rounds.recv())
        .await
        .expect("real resident cell did not reach the next model request")
        .expect("scripted provider closed")
}

async fn next_round_with_state(
    rounds: &mut mpsc::UnboundedReceiver<RequestedRound>,
    fixture: &RunningBrowserHost,
) -> RequestedRound {
    let root = fixture.campaign.actor.identity();
    let waiting = async {
        loop {
            tokio::select! {
                round = rounds.recv() => return round.expect("scripted provider closed"),
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    let graph = fixture.campaign.forest.inspect_host_graph();
                    if let Some(child) = graph.iter().find(|node| {
                        node.actor != root
                            && node.terminal.as_ref().is_some_and(|terminal| {
                                terminal.kind != exomonad_actor::ActorExitKind::Completed
                            })
                    }) {
                        panic!("native child exited before acceptance completed: {child:?}; graph={graph:?}");
                    }
                }
            }
        }
    };
    tokio::time::timeout(CELL_TIMEOUT, waiting)
        .await
        .unwrap_or_else(|_| {
            panic!(
                "native actor did not reach the next model request; graph={:?}",
                fixture.campaign.forest.inspect_host_graph()
            )
        })
}

fn retained_output(request: &ResponsesRequest, call_id: &str) -> Value {
    let item = request
        .input
        .iter()
        .find(|item| {
            matches!(
                item.0["type"].as_str(),
                Some("custom_tool_call_output" | "function_call_output")
            ) && item.0["call_id"] == call_id
        })
        .unwrap_or_else(|| panic!("next inference omitted terminal output for {call_id}"));
    serde_json::from_str(item.0["output"].as_str().unwrap()).unwrap()
}

fn successful_output(request: &ResponsesRequest, call_id: &str) -> Value {
    let output = retained_output(request, call_id);
    assert!(
        matches!(output["status"].as_str(), Some("completed" | "committed")),
        "{output}"
    );
    assert!(
        output["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["status"] == "committed"),
        "{output}"
    );
    output
}

fn context_operations(output: &Value) -> Vec<&Value> {
    output["items"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|item| item["operations"].as_array().into_iter().flatten())
        .filter(|operation| operation["effect"] == "context transformation")
        .collect()
}

fn assert_context_operations_uncommitted(output: &Value) {
    let committed =
        serde_json::to_value(tidepool_runtime::session::WorkbenchOperationDisposition::Committed)
            .unwrap();
    for operation in context_operations(output) {
        assert_ne!(operation["disposition"], committed, "{output}");
    }
}

fn settings() -> (tempfile::TempDir, crate::exomonad::EmbeddedLaunchConfig) {
    let files = tempfile::tempdir().unwrap();
    let assets = files.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
    let session_secret_file = files.path().join("secret");
    std::fs::write(&session_secret_file, SECRET).unwrap();
    let credential_file = files.path().join("unused-auth.json");
    std::fs::write(&credential_file, "{}").unwrap();
    (
        files,
        crate::exomonad::EmbeddedLaunchConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Https,
            public_origin: None,
            asset_root: assets,
            browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret,
            session_secret_file: Some(session_secret_file),
            provider: crate::exomonad::EmbeddedModelProvider::Codex,
            credential_file,
            context_capacity_tokens: 200_000,
            concurrent_jobs: 3,
        },
    )
}

async fn start() -> (
    tempfile::TempDir,
    RunningBrowserHost,
    mpsc::UnboundedReceiver<RequestedRound>,
) {
    start_with_spec(None).await
}

async fn start_with_spec(
    spec: Option<&str>,
) -> (
    tempfile::TempDir,
    RunningBrowserHost,
    mpsc::UnboundedReceiver<RequestedRound>,
) {
    let (files, settings) = settings();
    let (requests, rounds) = mpsc::unbounded_channel();
    let transport: Arc<dyn ResponsesTransport> = Arc::new(ScriptedProvider(requests));
    let fixture = RunningBrowserHost::start_configured(&settings, &transport, |config| {
        let authored = config.workspace.join(".exomonad");
        std::fs::create_dir_all(&authored).unwrap();
        let mut configuration =
            "[defaults]\nmodel='test-model'\n[models]\nexecutor='gpt-6.1-sol'\n".to_owned();
        if let Some(spec) = spec {
            std::fs::write(authored.join("AgentSpec.hs"), spec).unwrap();
            configuration.push_str("[haskell]\nsource_roots=['.']\nspec='AgentSpec.agentSpec'\n");
        }
        std::fs::write(authored.join("config.toml"), configuration).unwrap();
        test_campaign::commit_workspace(&config.workspace);
        config.workspace_inputs = Some(
            crate::exomonad::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root)
                .unwrap(),
        );
    })
    .await
    .unwrap();
    let actor = fixture.campaign.actor.identity();
    let origin = format!("https://{}", fixture.address);
    let api = format!("http://{}/api", fixture.address);
    let client = reqwest::Client::new();
    let login = client
        .post(format!("{api}/session"))
        .header("Origin", &origin)
        .json(&json!({"secret": SECRET}))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let cookie = login.headers()[reqwest::header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let input = client
        .post(format!("{api}/commands"))
        .header("Origin", &origin)
        .header(reqwest::header::COOKIE, cookie)
        .json(&harness::server::ClientCommand::Host {
            operation_id: harness::embedding::ClientOperationId(uuid::Uuid::new_v4()),
            command: harness::server::HostCommand::Input {
                target: harness::embedding::HostIdentity {
                    run: runtime_namespace(&fixture.campaign.config.run_root),
                    actor: AgentPath("/root".into()),
                    incarnation: actor.incarnation.0.to_string(),
                },
                text: "parent-original".into(),
            },
        })
        .send()
        .await
        .unwrap();
    assert_eq!(input.status(), reqwest::StatusCode::ACCEPTED);
    (files, fixture, rounds)
}

fn has_user_text(request: &ResponsesRequest, expected: &str) -> bool {
    // Edited text is projected with an attribution line before its authored body.
    let has_line = |text: &Value| {
        text.as_str()
            .is_some_and(|text| text.lines().any(|line| line == expected))
    };
    request.input.iter().any(|item| {
        item.0["role"] == "user"
            && (has_line(&item.0["content"])
                || item.0["content"]
                    .as_array()
                    .is_some_and(|parts| parts.iter().any(|part| has_line(&part["text"]))))
    })
}

fn root_context_state(fixture: &RunningBrowserHost) -> harness::context::ContextRequestState {
    let store = fixture.runtime.store();
    let identity = harness::embedding::HostIdentity {
        run: runtime_namespace(&fixture.campaign.config.run_root),
        actor: AgentPath("/root".into()),
        incarnation: fixture.campaign.actor.identity().incarnation.0.to_string(),
    };
    let head = store
        .embedded_agent_head(&identity)
        .unwrap()
        .expect("embedded actor has a committed conversation head");
    store
        .context_request_state(
            &head,
            &harness::model::ConversationIdentity::Embedded {
                run: identity.run,
                actor: identity.actor,
                incarnation: identity.incarnation,
            },
        )
        .unwrap()
}

#[tokio::test]
async fn resident_sync_context_commits_before_deferred_children_and_child_model_switch() {
    let (_files, fixture, mut rounds) = start().await;
    let root_identity = fixture.campaign.actor.identity();
    let setup = next_round(&mut rounds).await;
    assert!(setup.is_root());
    setup.cell(
        "context-setup",
        include_str!("fixtures/context_acceptance_setup.hs"),
    );
    let parent = next_round(&mut rounds).await;
    assert!(parent.is_root());
    successful_output(&parent.request, "context-setup");
    let root_session = parent.request.session_id.clone();
    parent.cell(
        "context-parent",
        include_str!("fixtures/context_acceptance_parent.hs"),
    );

    let mut child_sessions = BTreeSet::new();
    let mut root_committed = false;
    let mut children_committed = 0;
    while !root_committed || children_committed != 2 {
        let round = next_round_with_state(&mut rounds, &fixture).await;
        if round.is_root() {
            assert!(
                !root_committed,
                "parent inferred twice before another input"
            );
            assert_eq!(round.request.session_id, root_session);
            successful_output(&round.request, "context-parent");
            assert_eq!(round.request.model, "parent-curated-model");
            assert!(has_user_text(&round.request, "parent-curated"));
            assert!(!has_user_text(&round.request, "parent-original"));
            root_committed = true;
            round.finish();
        } else if child_sessions.insert(round.request.session_id.clone()) {
            assert!(child_sessions.len() <= 2, "launched an unexpected child");
            assert_eq!(round.request.model, "test-model");
            assert!(has_user_text(&round.request, "parent-curated"));
            assert!(!has_user_text(&round.request, "parent-original"));
            successful_output(&round.request, "context-parent");
            round.cell(
                "context-child",
                include_str!("fixtures/context_acceptance_child.hs"),
            );
        } else {
            assert_eq!(round.request.model, "gpt-6.1-sol");
            assert!(has_user_text(&round.request, "child-curated"));
            assert!(!has_user_text(&round.request, "parent-curated"));
            let output = successful_output(&round.request, "context-child");
            assert_eq!(
                output["items"].as_array().unwrap().last().unwrap()["output"],
                "42"
            );
            children_committed += 1;
            round.finish();
        }
    }
    assert_eq!(child_sessions.len(), 2);
    assert_eq!(fixture.campaign.actor.identity(), root_identity);
    assert!(fixture.campaign.actor.terminal().get().is_none());
    fixture.stop().await.unwrap();
}

#[tokio::test]
async fn resident_compiled_sync_handler_commits_context_and_model_before_inference() {
    let (_files, fixture, mut rounds) = start_with_spec(Some(include_str!(
        "fixtures/context_acceptance_agent_spec.hs"
    )))
    .await;
    let actor = fixture.campaign.actor.identity();
    let first = next_round(&mut rounds).await;
    assert!(first.is_root());
    assert!(
        has_user_text(&first.request, "parent-original"),
        "initial input: {:?}",
        first.request.input
    );
    assert!(first
        .request
        .tools
        .iter()
        .any(|tool| tool["name"] == "haskell"));
    assert!(first.request.tools.iter().any(|tool| {
        tool["name"] == "curate" && tool["type"] == "function" && tool["strict"] == true
    }));
    let session = first.request.session_id.clone();
    first.function("compiled-context", "curate", json!({"proceed": true}));
    let successor = next_round(&mut rounds).await;
    assert!(successor.is_root());
    let output = successful_output(&successor.request, "compiled-context");
    assert_eq!(output["total"], 1, "{output}");
    assert_eq!(output["nextIndex"], 1, "{output}");
    assert_eq!(
        output["items"][0]["output"], "compiled-handler-committed",
        "{output}"
    );
    assert_eq!(successor.request.session_id, session);
    assert_eq!(successor.request.model, "gpt-6.1-sol");
    assert!(
        has_user_text(&successor.request, "compiled-handler-curated"),
        "committed input: {:?}",
        successor.request.input
    );
    assert!(
        !has_user_text(&successor.request, "parent-original"),
        "committed input: {:?}",
        successor.request.input
    );
    let after = root_context_state(&fixture);
    assert_eq!(after.generation, 1);
    assert_eq!(after.model.as_deref(), Some("gpt-6.1-sol"));
    assert_eq!(fixture.campaign.actor.identity(), actor);
    assert!(fixture.campaign.actor.terminal().get().is_none());
    successor.finish();
    fixture.stop().await.unwrap();
}

#[tokio::test]
async fn resident_sync_context_failure_keeps_prefix_model_and_defers_children() {
    let (_files, fixture, mut rounds) = start().await;
    let setup = next_round(&mut rounds).await;
    setup.cell(
        "context-setup",
        include_str!("fixtures/context_acceptance_setup.hs"),
    );
    let parent = next_round(&mut rounds).await;
    successful_output(&parent.request, "context-setup");
    let before = root_context_state(&fixture);
    parent.cell(
        "context-failure",
        include_str!("fixtures/context_acceptance_failure.hs"),
    );
    let successor = next_round(&mut rounds).await;
    assert!(
        successor.is_root(),
        "failed invocation launched a deferred child"
    );
    assert_eq!(successor.request.model, "test-model");
    assert!(has_user_text(&successor.request, "parent-original"));
    assert!(!has_user_text(&successor.request, "must-not-publish"));
    let after = root_context_state(&fixture);
    assert_eq!(after.generation, before.generation);
    assert_eq!(after.model, before.model);
    let terminal = retained_output(&successor.request, "context-failure");
    assert!(terminal
        .to_string()
        .contains("intentional context transaction failure"));
    let staged =
        serde_json::to_value(tidepool_runtime::session::WorkbenchOperationDisposition::Staged)
            .unwrap();
    assert!(
        context_operations(&terminal)
            .iter()
            .any(|operation| operation["disposition"] == staged),
        "{terminal}"
    );
    assert_context_operations_uncommitted(&terminal);
    successor.finish();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), rounds.recv())
            .await
            .is_err()
    );
    fixture.stop().await.unwrap();
}

#[tokio::test]
async fn resident_sync_context_cancel_discards_staging_and_never_launches_children() {
    let (_files, fixture, mut rounds) = start().await;
    let root_identity = fixture.campaign.actor.identity();
    let setup = next_round(&mut rounds).await;
    setup.cell(
        "context-setup",
        include_str!("fixtures/context_acceptance_setup.hs"),
    );
    let parent = next_round(&mut rounds).await;
    successful_output(&parent.request, "context-setup");
    parent.cell(
        "context-cancel",
        include_str!("fixtures/context_acceptance_cancel.hs"),
    );
    let store = fixture.runtime.store();
    let identity = harness::embedding::HostIdentity {
        run: runtime_namespace(&fixture.campaign.config.run_root),
        actor: AgentPath("/root".into()),
        incarnation: fixture.campaign.actor.identity().incarnation.0.to_string(),
    };
    let operation = tokio::time::timeout(CELL_TIMEOUT, async {
        loop {
            let claims = store
                .claims(&harness::model::CallId("context-cancel".into()))
                .unwrap();
            if let Some(claim) = claims.first() {
                let context = exomonad_tool::ToolInvocationContext {
                    origin: exomonad_tool::ToolInvocationOrigin::Model(
                        embedded_harness::original_operation(&identity, &claim.operation).unwrap(),
                    ),
                    call_id: claim.operation.call.0.clone(),
                    namespace: None,
                };
                if fixture
                    .campaign
                    .actor
                    .hosted_workbench_waiting(&context)
                    .is_some()
                {
                    break claim.operation.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("staged cell never reached its real Sleep wait");
    let conversation_identity = harness::model::ConversationIdentity::Embedded {
        run: identity.run,
        actor: identity.actor,
        incarnation: identity.incarnation,
    };
    let before = store
        .context_request_state(&operation.request, &conversation_identity)
        .unwrap();
    fixture
        .runtime
        .scheduler()
        .cancel(&operation)
        .await
        .unwrap();
    let settled = tokio::time::timeout(
        Duration::from_secs(10),
        fixture.runtime.scheduler().wait(&operation),
    )
    .await
    .unwrap()
    .unwrap();
    match &settled {
        harness::turn::JobOutput::Cancelled => {}
        harness::turn::JobOutput::CancelledWithReceipt(Ok(receipt)) => {
            assert_context_operations_uncommitted(receipt);
        }
        harness::turn::JobOutput::CancelledWithReceipt(Err(failure)) => {
            if let Some(metadata) = failure.metadata() {
                assert_context_operations_uncommitted(metadata);
            }
        }
        _ => panic!("expected confirmed cancellation, received {settled:?}"),
    }
    let successor = next_round(&mut rounds).await;
    assert!(
        successor.is_root(),
        "cancelled invocation launched a deferred child"
    );
    assert_eq!(successor.request.model, "test-model");
    assert!(has_user_text(&successor.request, "parent-original"));
    assert!(!has_user_text(&successor.request, "must-not-publish"));
    let terminal = retained_output(&successor.request, "context-cancel");
    assert_context_operations_uncommitted(&terminal);
    if let Some(receipt) = terminal.get("receipt") {
        assert_context_operations_uncommitted(receipt);
    }
    let after = root_context_state(&fixture);
    assert_eq!(after.generation, before.generation);
    assert_eq!(after.model, before.model);
    assert_eq!(fixture.campaign.actor.identity(), root_identity);
    assert!(fixture.campaign.actor.terminal().get().is_none());
    let prefix = |history: Vec<(harness::model::RequestId, harness::item::ItemHash, Item)>| {
        history
            .into_iter()
            .filter(|(_, _, item)| item.0["call_id"] != "context-cancel")
            .collect::<Vec<_>>()
    };
    assert_eq!(prefix(after.history), prefix(before.history));
    successor.finish();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), rounds.recv())
            .await
            .is_err()
    );
    fixture.stop().await.unwrap();
}
