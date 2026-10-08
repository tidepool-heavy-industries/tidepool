use super::hosted_test_context::HostedTestRuntime;
use super::*;
use async_trait::async_trait;
use harness::{
    engine::ResponsesTransport,
    item::{Item, ToolExecution},
    model::AgentPath,
    transport::{ResponsesRequest, ResponsesTurn, TransportError},
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{mpsc, oneshot};

const CELL_TIMEOUT: Duration = Duration::from_secs(300);
const SECRET: &str = "resident-context-acceptance-secret-32-bytes";

struct RequestedRound {
    request: ResponsesRequest,
    reply: oneshot::Sender<ResponsesTurn>,
    provider_calls: Arc<AtomicUsize>,
}

impl RequestedRound {
    fn is_root(&self) -> bool {
        self.request.session_id.contains(":/root:")
    }

    fn cell(self, call_id: &str, source: &str) {
        self.cell_named("haskell_sync", call_id, source, ToolExecution::Synchronous);
    }

    fn async_cell(self, call_id: &str, source: &str) {
        self.cell_named("haskell", call_id, source, ToolExecution::Asynchronous);
    }

    fn cell_named(self, name: &str, call_id: &str, source: &str, execution: ToolExecution) {
        let call = Item(json!({
            "type": "custom_tool_call", "call_id": call_id,
            "name": name, "input": source,
            "async": execution == ToolExecution::Asynchronous,
        }));
        assert_eq!(call.tool_call().unwrap().unwrap().execution, execution);
        self.reply
            .send(ResponsesTurn {
                response_id: format!("response-{call_id}"),
                items: vec![call],
                usage: Default::default(),
            })
            .expect("resident Engine still awaits its scripted reply");
    }

    fn cell_with_reasoning(self, call_id: &str, source: &str) {
        self.respond_with_reasoning(
            call_id,
            Item(json!({
                "type": "custom_tool_call", "call_id": call_id,
                "name": "haskell_sync", "input": source,
            })),
        );
    }

    fn function_with_reasoning(self, call_id: &str, name: &str, arguments: Value) {
        self.respond_with_reasoning(
            call_id,
            Item(json!({
                "type": "function_call", "call_id": call_id,
                "name": name, "arguments": arguments.to_string(),
            })),
        );
    }

    fn respond_with_reasoning(self, call_id: &str, call: Item) {
        self.reply
            .send(ResponsesTurn {
                response_id: format!("response-{call_id}"),
                items: vec![reasoning_item(call_id), call],
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

    fn wait_for_pending(self) {
        let call_id = uuid::Uuid::new_v4().simple().to_string();
        self.reply
            .send(ResponsesTurn {
                response_id: format!("wait-{call_id}"),
                items: vec![Item(json!({
                    "type": "function_call", "call_id": call_id,
                    "name": "yield", "arguments": "{}",
                }))],
                usage: Default::default(),
            })
            .expect("resident Engine still awaits its scripted reply");
    }
}

struct ScriptedProvider {
    requests: mpsc::UnboundedSender<RequestedRound>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl ResponsesTransport for ScriptedProvider {
    async fn create(&self, request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        let (reply, completed) = oneshot::channel();
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests
            .send(RequestedRound {
                request,
                reply,
                provider_calls: Arc::clone(&self.calls),
            })
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

async fn next_round_with_output(
    rounds: &mut mpsc::UnboundedReceiver<RequestedRound>,
    call_id: &str,
) -> RequestedRound {
    tokio::time::timeout(CELL_TIMEOUT, async {
        loop {
            let round = next_round(rounds).await;
            assert!(round.is_root(), "expected root output for {call_id}");
            if output_item(&round.request.input, call_id).is_some() {
                return round;
            }
            // The built-in wait observes the real settlement wake, including a
            // completion that races this already-issued provider request.
            round.wait_for_pending();
        }
    })
    .await
    .unwrap_or_else(|_| panic!("native cell {call_id} did not publish its actual terminal output"))
}

async fn next_round_with_state(
    rounds: &mut mpsc::UnboundedReceiver<RequestedRound>,
    fixture: &mut HostedTestRuntime,
) -> RequestedRound {
    let root = fixture.context.actor.identity();
    let forest = Arc::clone(&fixture.context.forest);
    let waiting = async {
        loop {
            tokio::select! {
                biased;
                outcome = fixture.host_outcome() => {
                    panic!("embedded host stopped during child acceptance: {outcome:?}; graph={:?}", forest.inspect_host_graph());
                }
                round = rounds.recv() => match round {
                    Some(round) => return round,
                    None => {
                        let host = tokio::time::timeout(Duration::from_secs(35), fixture.host_outcome()).await;
                        panic!("scripted provider closed; host={host:?}; graph={:?}", forest.inspect_host_graph());
                    }
                },
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    let graph = forest.inspect_host_graph();
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
                forest.inspect_host_graph()
            )
        })
}

fn retained_output(request: &ResponsesRequest, call_id: &str) -> Value {
    retained_output_items(&request.input, call_id)
}

// Hold both provider replies so actor-owned child survival is observed after
// the parent invocation settles, regardless of which inference arrives first.
async fn root_and_child_rounds(
    rounds: &mut mpsc::UnboundedReceiver<RequestedRound>,
    fixture: &mut HostedTestRuntime,
    child: Option<RequestedRound>,
) -> (RequestedRound, RequestedRound) {
    root_and_child_rounds_with_output(rounds, fixture, child, None).await
}

async fn root_and_child_rounds_with_output(
    rounds: &mut mpsc::UnboundedReceiver<RequestedRound>,
    fixture: &mut HostedTestRuntime,
    mut child: Option<RequestedRound>,
    root_output: Option<&str>,
) -> (RequestedRound, RequestedRound) {
    let mut root = None;
    while root.is_none() || child.is_none() {
        let round = next_round_with_state(rounds, fixture).await;
        if round.is_root()
            && root_output
                .is_some_and(|call_id| output_item(&round.request.input, call_id).is_none())
        {
            round.wait_for_pending();
            continue;
        }
        let slot = if round.is_root() {
            &mut root
        } else {
            &mut child
        };
        assert!(slot.is_none(), "an unanswered actor inferred twice");
        *slot = Some(round);
    }
    (root.unwrap(), child.unwrap())
}

fn assert_before_call_prefix(request: &ResponsesRequest, raw: &[Item], call_id: &str) {
    assert!(raw.iter().all(|item| item.0["call_id"] != call_id));
    assert!(
        request
            .input
            .iter()
            .all(|item| item.0["call_id"] != call_id)
    );
    assert!(has_user_text(request, "parent-original"));
    assert!(!has_user_text(request, "must-not-publish"));
}

async fn prove_child_survives(
    child: RequestedRound,
    rounds: &mut mpsc::UnboundedReceiver<RequestedRound>,
    fixture: &mut HostedTestRuntime,
    parent_call: &str,
    setup_call: &str,
    original_setup: &Item,
) {
    let root = fixture.context.actor.identity();
    let graph = fixture.context.forest.inspect_host_graph();
    let children = graph
        .iter()
        .filter(|node| node.model_actor && node.creator == Some(root))
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 1, "unexpected child admissions: {graph:?}");
    assert!(children[0].terminal.is_none(), "child retired: {graph:?}");
    let raw = raw_request_items(fixture, &child.request);
    assert_before_call_prefix(&child.request, &raw, parent_call);
    assert_eq!(child.request.model, "test-model");
    assert_eq!(retained_output_item(&raw, setup_call), original_setup);
    assert_eq!(
        retained_output_item(&child.request.input, setup_call),
        original_setup
    );
    let session = child.request.session_id.clone();
    child.cell(
        "surviving-child-reuse",
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/context_acceptance_after_failure.hs",
        ),
    );
    let reused = next_round_with_state(rounds, fixture).await;
    assert!(!reused.is_root());
    assert_eq!(reused.request.session_id, session);
    let raw = raw_request_items(fixture, &reused.request);
    assert_before_call_prefix(&reused.request, &raw, parent_call);
    let output = successful_output_items(&raw, "surviving-child-reuse");
    assert_eq!(test_campaign::explicit_display_text(&output), "42");
    reused.finish();
}

fn retained_output_items(items: &[Item], call_id: &str) -> Value {
    let item = retained_output_item(items, call_id);
    serde_json::from_str(item.0["output"].as_str().unwrap()).unwrap()
}

fn retained_output_item<'a>(items: &'a [Item], call_id: &str) -> &'a Item {
    output_item(items, call_id)
        .unwrap_or_else(|| panic!("next inference omitted terminal output for {call_id}"))
}

fn output_item<'a>(items: &'a [Item], call_id: &str) -> Option<&'a Item> {
    items.iter().find(|item| {
        matches!(
            item.0["type"].as_str(),
            Some("custom_tool_call_output" | "function_call_output")
        ) && item.0["call_id"] == call_id
    })
}

fn successful_output(request: &ResponsesRequest, call_id: &str) -> Value {
    successful_output_items(&request.input, call_id)
}

fn successful_output_items(items: &[Item], call_id: &str) -> Value {
    let output = retained_output_items(items, call_id);
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

fn reasoning_item(call_id: &str) -> Item {
    Item(json!({
        "type": "reasoning", "id": format!("reasoning-{call_id}"),
        "encrypted_content": format!("opaque-{call_id}"),
        "summary": [{"type": "summary_text", "text": format!("visible reasoning for {call_id}")}],
    }))
}

fn raw_request_items(fixture: &HostedTestRuntime, request: &ResponsesRequest) -> Vec<Item> {
    raw_request_history(fixture, request)
        .into_iter()
        .map(|(_, _, item)| item)
        .collect()
}

fn raw_request_history(
    fixture: &HostedTestRuntime,
    request: &ResponsesRequest,
) -> Vec<(harness::model::RequestId, harness::item::ItemHash, Item)> {
    let run = runtime_namespace(&fixture.context.config.run_directory.path());
    let actor_and_incarnation = request
        .session_id
        .strip_prefix(&format!("{run}:"))
        .expect("request belongs to this embedded run");
    let (actor, incarnation) = actor_and_incarnation
        .rsplit_once(':')
        .expect("embedded request carries its exact incarnation");
    let store = fixture.runtime.store();
    let frontier = store
        .embedded_round_frontier(&harness::embedding::HostIdentity {
            run,
            actor: AgentPath(actor.to_owned()),
            incarnation: incarnation.to_owned(),
        })
        .unwrap();
    let head = frontier
        .pending_head
        .or(frontier.settled_head)
        .expect("the observed provider request has admitted history");
    store.context_history(&head).unwrap()
}

fn assert_portable_exchange(request: &ResponsesRequest, raw: &[Item], call_id: &str, tool: &str) {
    assert!(
        request
            .input
            .iter()
            .all(|item| item.0.get("encrypted_content").is_none()),
        "foreign encrypted reasoning escaped into {} input: {:?}",
        request.model,
        request.input
    );
    assert!(
        request
            .input
            .iter()
            .all(|item| item.0["call_id"] != call_id),
        "foreign native call/output for {call_id} escaped into {} input: {:?}",
        request.model,
        request.input
    );
    let input = serde_json::to_string(&request.input).unwrap();
    assert!(!input.contains(&format!("opaque-{call_id}")), "{input}");
    let summary = format!("visible reasoning for {call_id}");
    let notes = request
        .input
        .iter()
        .filter(|item| item.0["type"] == "message" && item.0["role"] == "assistant")
        .filter_map(|item| item.0["content"].as_str())
        .filter(|text| text.contains(&summary))
        .collect::<Vec<_>>();
    assert_eq!(notes.len(), 1, "{input}");
    let note = notes[0];
    let call = raw
        .iter()
        .find(|item| {
            matches!(
                item.0["type"].as_str(),
                Some("custom_tool_call" | "function_call")
            ) && item.0["call_id"] == call_id
        })
        .unwrap_or_else(|| panic!("raw history omitted issuing call {call_id}"));
    assert_eq!(call.0["name"], tool);
    let arguments = match call.0["type"].as_str().unwrap() {
        "custom_tool_call" => &call.0["input"],
        "function_call" => &call.0["arguments"],
        _ => unreachable!(),
    };
    let output = &retained_output_item(raw, call_id).0["output"];
    for visible in [
        json!(call_id),
        json!(tool),
        arguments.clone(),
        output.clone(),
    ] {
        let expected = serde_json::to_string(&visible).unwrap();
        assert!(note.contains(&expected), "missing {expected} in {note}");
    }
}

fn assert_native_exchange_preserved(request: &ResponsesRequest, raw: &[Item], call_id: &str) {
    let reasoning = reasoning_item(call_id);
    assert!(raw.contains(&reasoning));
    assert!(request.input.contains(&reasoning));
    let call = raw
        .iter()
        .find(|item| {
            matches!(
                item.0["type"].as_str(),
                Some("custom_tool_call" | "function_call")
            ) && item.0["call_id"] == call_id
        })
        .unwrap_or_else(|| panic!("canonical history omitted native call {call_id}"));
    assert!(
        request.input.contains(call),
        "projection changed native call {call_id}: {:?}",
        request.input
    );
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
    HostedTestRuntime,
    mpsc::UnboundedReceiver<RequestedRound>,
) {
    let spec = tidepool_testing::fixture_source(
        "bridge/facade/src/actor_host/fixtures/context_acceptance_agent_spec.hs",
    );
    start_with_spec_and_model(Some(&spec), "test-model").await
}

async fn start_with_spec(
    spec: Option<&str>,
) -> (
    tempfile::TempDir,
    HostedTestRuntime,
    mpsc::UnboundedReceiver<RequestedRound>,
) {
    let initial_model = if spec.is_some() {
        "gpt-6.1-sol"
    } else {
        "test-model"
    };
    start_with_spec_and_model(spec, initial_model).await
}

async fn start_with_spec_and_model(
    spec: Option<&str>,
    initial_model: &str,
) -> (
    tempfile::TempDir,
    HostedTestRuntime,
    mpsc::UnboundedReceiver<RequestedRound>,
) {
    let (files, settings) = settings();
    let (requests, rounds) = mpsc::unbounded_channel();
    let transport: Arc<dyn ResponsesTransport> = Arc::new(ScriptedProvider {
        requests,
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let fixture = HostedTestRuntime::start_configured(&settings, &transport, |config| {
        let authored = config.workspace.join(".exomonad");
        std::fs::create_dir_all(&authored).unwrap();
        config.model = initial_model.into();
        if let Some(spec) = spec {
            std::fs::write(authored.join("AgentSpec.hs"), spec).unwrap();
            std::fs::write(
                authored.join("ContextWorkflow.hs"),
                &tidepool_testing::fixture_source(
                    "bridge/haskell/examples/model-turns/ContextWorkflow.hs",
                ),
            )
            .unwrap();
            // ContextWorkflow imports Jev.Operators and Jev.Tidepool. Keep the
            // same workspace-owned front and pinned core source that projects
            // receive, so the compiled acceptance fixture has real provenance.
            let operators = authored.join("Jev/Operators.hs");
            std::fs::create_dir_all(operators.parent().unwrap()).unwrap();
            std::fs::write(
                operators,
                &tidepool_testing::fixture_source(
                    "exomonad/examples/workspace/.exomonad/Jev/Operators.hs",
                ),
            )
            .unwrap();
            for (name, contents) in [
                (
                    "flake.nix",
                    include_str!("../../../../exomonad/examples/workspace/flake.nix"),
                ),
                (
                    "flake.lock",
                    include_str!("../../../../exomonad/examples/workspace/flake.lock"),
                ),
            ] {
                std::fs::write(config.workspace.join(name), contents).unwrap();
            }
        }
        crate::exomonad::write_fixture_project_config(&authored, initial_model, |project| {
            project
                .models
                .insert("executor".into(), "gpt-6.1-sol".into());
            if spec.is_some() {
                project.haskell.source_roots = vec![".".into()];
                project.haskell.spec = Some("AgentSpec.agentSpec".into());
                project
                    .haskell
                    .flake_sources
                    .insert("jev-dsl".into(), vec!["core".into()]);
            }
        });
        test_campaign::commit_workspace(&config.workspace);
        config.workspace_inputs = Some(
            crate::exomonad::workspace::FrozenWorkspace::load(
                &config.workspace,
                &config.run_directory.path(),
            )
            .unwrap(),
        );
    })
    .await
    .unwrap();
    let actor = fixture.context.actor.identity();
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
                    run: runtime_namespace(&fixture.context.config.run_directory.path()),
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

#[tokio::test]
async fn native_notebook_scheduling_preserves_effects_and_published_bindings() {
    let (_files, fixture, mut rounds) = start_with_spec(None).await;
    let actor = fixture.context.actor.identity();
    let first = next_round(&mut rounds).await;
    let session = first.request.session_id.clone();
    first.cell(
        "first-sync-publication",
        "let notebookAction = (pure (41 :: Int) :: Eff effects Int)\n\
         let notebookSamples = [1, 2, 3] :: [Int]\n\
         notebookAction >>= display",
    );
    let following = next_round(&mut rounds).await;
    let output = successful_output(&following.request, "first-sync-publication");
    assert_eq!(test_campaign::explicit_display_text(&output), "41");
    following.async_cell(
        "async-publication",
        "let asyncValue = sum notebookSamples - 5 :: Int\n\
         let asyncAction = (do { value <- notebookAction; pure (value + asyncValue) } :: Eff effects Int)\n\
         asyncAction >>= display",
    );
    let following = next_round_with_output(&mut rounds, "async-publication").await;
    let output = successful_output(&following.request, "async-publication");
    assert_eq!(test_campaign::explicit_display_text(&output), "42");
    following.cell(
        "sync-publication",
        "let syncAction = (do { value <- asyncAction; pure (value + 1) } :: Eff effects Int)\n\
         syncAction >>= display",
    );
    let following = next_round(&mut rounds).await;
    let output = successful_output(&following.request, "sync-publication");
    assert_eq!(test_campaign::explicit_display_text(&output), "43");
    following.async_cell(
        "async-reuse",
        "syncAction >>= \\value -> display (value + 1)",
    );
    let following = next_round_with_output(&mut rounds, "async-reuse").await;
    let output = successful_output(&following.request, "async-reuse");
    assert_eq!(test_campaign::explicit_display_text(&output), "44");
    assert_eq!(following.request.session_id, session);
    assert_eq!(fixture.context.actor.identity(), actor);
    following.finish();
    fixture.stop().await.unwrap();
}

fn has_user_text(request: &ResponsesRequest, expected: &str) -> bool {
    user_text_occurrences(request, expected) != 0
}

fn user_text_occurrences(request: &ResponsesRequest, expected: &str) -> usize {
    // Match the authored line in flat or structured provider message bodies.
    let count_lines = |text: &Value| {
        text.as_str().map_or(0, |text| {
            text.lines().filter(|line| *line == expected).count()
        })
    };
    request
        .input
        .iter()
        .filter(|item| item.0["role"] == "user")
        .map(|item| {
            count_lines(&item.0["content"])
                + item.0["content"].as_array().map_or(0, |parts| {
                    parts
                        .iter()
                        .map(|part| count_lines(&part["text"]))
                        .sum::<usize>()
                })
        })
        .sum()
}

fn root_context_state(fixture: &HostedTestRuntime) -> harness::context::ContextRequestState {
    let store = fixture.runtime.store();
    let identity = harness::embedding::HostIdentity {
        run: runtime_namespace(&fixture.context.config.run_directory.path()),
        actor: AgentPath("/root".into()),
        incarnation: fixture.context.actor.identity().incarnation.0.to_string(),
    };
    // The scripted successor is admitted but has not returned a final answer.
    // Its current prefix belongs to the pending frontier, before round settlement.
    let frontier = store.embedded_round_frontier(&identity).unwrap();
    let head = frontier
        .pending_head
        .or(frontier.settled_head)
        .expect("embedded actor has an admitted conversation head");
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
async fn resident_fresh_spawn_installs_idle_without_inference_until_explicit_request() {
    const SEED: &str = "fresh-idle-context-seed";
    const INPUT: &str = "explicit-idle-child-request";
    let (_files, mut fixture, mut rounds) = start().await;
    let initial = next_round(&mut rounds).await;
    assert!(initial.is_root());
    initial.cell(
        "fresh-idle-spawn",
        include_str!("fixtures/context_acceptance_idle_spawn.hs"),
    );
    let installed = next_round_with_state(&mut rounds, &mut fixture).await;
    assert!(
        installed.is_root(),
        "spawn activated a provider before the first request"
    );
    let output = successful_output(&installed.request, "fresh-idle-spawn");
    assert!(test_campaign::explicit_display_text(&output).contains("installed-idle"));
    let root = fixture.context.actor.identity();
    let graph = fixture.context.forest.inspect_host_graph();
    let children = graph
        .iter()
        .filter(|node| node.model_actor && node.creator == Some(root))
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 1, "{graph:?}");
    let child = children[0];
    assert!(child.terminal.is_none());
    assert_eq!(child.workbench, exomonad_actor::ActorWorkbenchPosture::Idle);
    assert!(child.active_requests.is_empty());
    assert!(child.queued_requests.is_empty());
    assert!(
        child.provider_turn.is_none(),
        "idle spawn inferred: {child:?}"
    );
    assert_eq!(child.context_parent, None);
    let binding = fixture
        .context
        .binding(child.actor)
        .expect("spawn has attached its provider");
    let conversation = binding
        .conversation()
        .expect("spawn has installed its conversation");
    let child_path = conversation.identity().actor.clone();
    let installation = fixture.context.observer.installation(child.actor).await;
    assert!(
        installation
            .tools
            .iter()
            .any(|tool| tool.name() == "haskell_sync")
    );
    // The real spawn call has completed and its installation is observable;
    // the only provider invocations so far are the two root rounds we hold.
    assert_eq!(installed.provider_calls.load(Ordering::SeqCst), 2);
    assert!(matches!(
        rounds.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));

    installed.cell(
        "fresh-idle-activate",
        include_str!("fixtures/context_acceptance_idle_request.hs"),
    );
    let activated = next_round_with_state(&mut rounds, &mut fixture).await;
    assert!(
        !activated.is_root(),
        "parent must await the typed child response"
    );
    assert!(
        activated
            .request
            .session_id
            .contains(&format!(":{}:", child_path.0))
    );
    assert_eq!(user_text_occurrences(&activated.request, SEED), 1);
    assert!(!has_user_text(&activated.request, "parent-original"));
    let child_session = activated.request.session_id.clone();
    activated.cell("fresh-idle-reply", "respond (sessionInput :: Text)");
    let (completed, replied) = root_and_child_rounds(&mut rounds, &mut fixture, None).await;
    assert_eq!(replied.request.session_id, child_session);
    assert_eq!(
        retained_output(&replied.request, "fresh-idle-reply")["status"],
        "replied"
    );
    assert_eq!(user_text_occurrences(&replied.request, SEED), 1);
    let output = successful_output(&completed.request, "fresh-idle-activate");
    assert_eq!(test_campaign::explicit_display_text(&output), INPUT);
    replied.finish();
    completed.finish();
    fixture.stop().await.unwrap();
}

#[tokio::test]
async fn resident_sync_native_trim_commits_atomically_and_children_reuse_captured_bindings() {
    const TRIMMED: &str = "[Trimmed: repetitive build output]\nBuild succeeded.";
    let (_files, mut fixture, mut rounds) = start().await;
    let setup = next_round(&mut rounds).await;
    setup.cell_with_reasoning(
        "native-trim-setup",
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/context_acceptance_native_trim_setup.hs",
        ),
    );
    let parent = next_round(&mut rounds).await;
    assert!(parent.is_root());
    let setup_output = successful_output(&parent.request, "native-trim-setup");
    assert!(
        test_campaign::explicit_display_text(&setup_output).ends_with("native-trim-result-tail"),
        "the full native trim result must be explicitly emitted: {setup_output}"
    );
    let original_output = retained_output_item(&parent.request.input, "native-trim-setup").clone();
    let original_output_hash = raw_request_history(&fixture, &parent.request)
        .into_iter()
        .find(|(_, _, item)| item == &original_output)
        .expect("setup result has canonical history")
        .1;
    assert!(
        original_output.0["output"]
            .as_str()
            .unwrap()
            .contains("native-trim-result-tail")
    );
    let before = root_context_state(&fixture);
    parent.cell_with_reasoning(
        "native-trim-parent",
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/context_acceptance_native_trim_parent.hs",
        ),
    );

    let mut child_sessions = BTreeSet::new();
    let mut root_committed = false;
    let mut children_committed = 0;
    while !root_committed || children_committed != 2 {
        let round = next_round_with_state(&mut rounds, &mut fixture).await;
        let raw = raw_request_items(&fixture, &round.request);
        assert_eq!(round.request.model, "test-model");
        assert_native_exchange_preserved(&round.request, &raw, "native-trim-setup");
        assert_eq!(
            retained_output_item(&raw, "native-trim-setup"),
            &original_output,
            "trimming altered the canonical result"
        );
        let output_hash = raw_request_history(&fixture, &round.request)
            .into_iter()
            .find(|(_, _, item)| item == &original_output)
            .expect("trimmed result retains its canonical history")
            .1;
        assert_eq!(output_hash, original_output_hash);
        if round.is_root() {
            assert!(
                !root_committed,
                "parent inferred twice before another input"
            );
            successful_output_items(&raw, "native-trim-parent");
            assert_native_exchange_preserved(&round.request, &raw, "native-trim-parent");
            let projected_output = retained_output_item(&round.request.input, "native-trim-setup");
            let mut expected_output = original_output.clone();
            expected_output.0["output"] = json!(TRIMMED);
            assert_eq!(projected_output, &expected_output);
            assert_eq!(
                round
                    .request
                    .input
                    .iter()
                    .rev()
                    .find_map(Item::configuration_effort),
                Some(harness::model::Effort::High)
            );
            let after = root_context_state(&fixture);
            assert_eq!(after.generation, before.generation + 1);
            assert_eq!(after.model.as_deref(), Some("test-model"));
            root_committed = true;
            round.finish();
        } else if child_sessions.insert(round.request.session_id.clone()) {
            assert!(child_sessions.len() <= 2, "launched an unexpected child");
            assert_before_call_prefix(&round.request, &raw, "native-trim-parent");
            assert!(raw.contains(&reasoning_item("native-trim-parent")));
            assert_eq!(
                retained_output_item(&round.request.input, "native-trim-setup"),
                &original_output
            );
            assert_eq!(
                round
                    .request
                    .input
                    .iter()
                    .rev()
                    .find_map(Item::configuration_effort),
                before
                    .history
                    .iter()
                    .rev()
                    .find_map(|(_, _, item)| item.configuration_effort())
            );
            round.cell(
                "native-trim-child",
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/fixtures/context_acceptance_native_trim_child.hs",
                ),
            );
        } else {
            assert_before_call_prefix(&round.request, &raw, "native-trim-parent");
            assert_eq!(
                retained_output_item(&round.request.input, "native-trim-setup"),
                &original_output
            );
            successful_output_items(&raw, "native-trim-child");
            children_committed += 1;
            round.finish();
        }
    }
    assert_eq!(child_sessions.len(), 2);
    fixture.stop().await.unwrap();
}

#[tokio::test]
async fn resident_sync_notes_commit_atomically_without_changing_captured_children() {
    let (_files, mut fixture, mut rounds) = start().await;
    let root_identity = fixture.context.actor.identity();
    let setup = next_round(&mut rounds).await;
    assert!(setup.is_root());
    setup.cell_with_reasoning(
        "context-setup",
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/context_acceptance_setup.hs",
        ),
    );
    let parent = next_round(&mut rounds).await;
    assert!(parent.is_root());
    successful_output(&parent.request, "context-setup");
    let root_session = parent.request.session_id.clone();
    parent.cell_with_reasoning(
        "context-parent",
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/context_acceptance_parent.hs",
        ),
    );

    let mut child_sessions = BTreeSet::new();
    let mut root_committed = false;
    let mut children_committed = 0;
    while !root_committed || children_committed != 2 {
        let round = next_round_with_state(&mut rounds, &mut fixture).await;
        if round.is_root() {
            assert!(
                !root_committed,
                "parent inferred twice before another input"
            );
            assert_eq!(round.request.session_id, root_session);
            let raw = raw_request_items(&fixture, &round.request);
            successful_output_items(&raw, "context-parent");
            assert!(raw.contains(&reasoning_item("context-parent")));
            assert_portable_exchange(&round.request, &raw, "context-parent", "haskell_sync");
            assert_eq!(round.request.model, "parent-curated-model");
            assert!(has_user_text(&round.request, "parent-curated"));
            assert!(has_user_text(&round.request, "parent-original"));
            root_committed = true;
            round.finish();
        } else if child_sessions.insert(round.request.session_id.clone()) {
            assert!(child_sessions.len() <= 2, "launched an unexpected child");
            assert_eq!(round.request.model, "child-preparation-model");
            assert!(!has_user_text(&round.request, "parent-curated"));
            assert!(has_user_text(&round.request, "parent-original"));
            let raw = raw_request_items(&fixture, &round.request);
            assert_before_call_prefix(&round.request, &raw, "context-parent");
            assert!(raw.contains(&reasoning_item("context-parent")));
            assert_portable_exchange(&round.request, &raw, "context-setup", "haskell_sync");
            round.cell_with_reasoning(
                "context-child",
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/fixtures/context_acceptance_child.hs",
                ),
            );
        } else {
            let raw = raw_request_items(&fixture, &round.request);
            assert_before_call_prefix(&round.request, &raw, "context-parent");
            successful_output_items(&raw, "context-child");
            assert!(raw.contains(&reasoning_item("context-child")));
            assert_portable_exchange(&round.request, &raw, "context-child", "haskell_sync");
            assert_eq!(round.request.model, "gpt-6.1-sol");
            assert!(has_user_text(&round.request, "child-curated"));
            assert!(!has_user_text(&round.request, "parent-curated"));
            assert!(has_user_text(&round.request, "parent-original"));
            children_committed += 1;
            round.finish();
        }
    }
    assert_eq!(child_sessions.len(), 2);
    assert_eq!(fixture.context.actor.identity(), root_identity);
    assert!(fixture.context.actor.terminal().get().is_none());
    fixture.stop().await.unwrap();
}

#[tokio::test]
async fn resident_compiled_sync_handler_commits_context_and_model_before_inference() {
    let spec = tidepool_testing::fixture_source(
        "bridge/facade/src/actor_host/fixtures/context_acceptance_agent_spec.hs",
    );
    let (_files, fixture, mut rounds) = start_with_spec(Some(&spec)).await;
    let actor = fixture.context.actor.identity();
    let first = next_round(&mut rounds).await;
    assert!(first.is_root());
    assert!(
        has_user_text(&first.request, "parent-original"),
        "initial input: {:?}",
        first.request.input
    );
    assert!(
        first
            .request
            .tools
            .iter()
            .any(|tool| tool["name"] == "haskell")
    );
    assert!(
        first
            .request
            .tools
            .iter()
            .any(|tool| tool["name"] == "haskell_sync")
    );
    assert!(first.request.tools.iter().any(|tool| {
        tool["name"] == "curate" && tool["type"] == "function" && tool["strict"] == true
    }));
    let session = first.request.session_id.clone();
    assert_eq!(first.request.model, "gpt-6.1-sol");
    assert_eq!(first.request.pinned_effort, harness::model::Effort::Low);
    first.function_with_reasoning("compiled-context", "curate", json!({"proceed": true}));
    let successor = next_round(&mut rounds).await;
    assert!(successor.is_root());
    let raw = raw_request_items(&fixture, &successor.request);
    let output = successful_output_items(&raw, "compiled-context");
    assert_native_exchange_preserved(&successor.request, &raw, "compiled-context");
    assert_eq!(output["total"], 1, "{output}");
    assert_eq!(output["nextIndex"], 1, "{output}");
    assert_eq!(
        output["items"][0]["output"], "compiled-handler-committed",
        "{output}"
    );
    assert_eq!(successor.request.session_id, session);
    assert_eq!(successor.request.model, "gpt-6.1-sol");
    assert_eq!(successor.request.pinned_effort, harness::model::Effort::Low);
    assert_eq!(
        successor
            .request
            .input
            .iter()
            .rev()
            .find_map(Item::configuration_effort),
        Some(harness::model::Effort::High)
    );
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
    assert_eq!(fixture.context.actor.identity(), actor);
    assert!(fixture.context.actor.terminal().get().is_none());
    successor.cell(
        "inspect-context",
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/context_acceptance_inspect.hs",
        ),
    );
    let inspected = next_round(&mut rounds).await;
    assert!(inspected.is_root());
    assert_eq!(inspected.request.session_id, session);
    assert_eq!(inspected.request.model, "gpt-6.1-sol");
    let output = successful_output(&inspected.request, "inspect-context");
    let displayed = output["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["output"].as_str())
        .filter(|text| text.contains("Context") && text.contains("blocks"))
        .collect::<Vec<_>>();
    assert_eq!(displayed.len(), 2, "{output}");
    assert!(
        displayed[0].contains("compiled-handler-curated"),
        "{output}"
    );
    assert!(
        displayed[1].contains("compiled-handler-curated"),
        "{output}"
    );
    assert!(displayed[1].contains("Child finding:"), "{output}");
    assert!(has_user_text(
        &inspected.request,
        "compiled-handler-curated"
    ));
    assert!(has_user_text(
        &inspected.request,
        "Child finding: The cache key must include the selected transcript prefix."
    ));
    assert_eq!(root_context_state(&fixture).generation, 2);
    assert_eq!(fixture.context.actor.identity(), actor);
    assert!(fixture.context.actor.terminal().get().is_none());
    inspected.finish();
    fixture.stop().await.unwrap();
}

#[tokio::test]
async fn resident_sync_context_failure_rolls_back_edits_and_keeps_activated_child() {
    let (_files, mut fixture, mut rounds) = start().await;
    let setup = next_round(&mut rounds).await;
    setup.cell(
        "context-setup",
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/context_acceptance_setup.hs",
        ),
    );
    let parent = next_round(&mut rounds).await;
    let setup_output = successful_output(&parent.request, "context-setup");
    assert_eq!(
        test_campaign::explicit_display_text(&setup_output),
        "context-setup-retained-result",
        "staging trims the setup cell's genuine native output"
    );
    let original_setup = retained_output_item(&parent.request.input, "context-setup").clone();
    let before = root_context_state(&fixture);
    parent.cell_with_reasoning(
        "context-failure",
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/context_acceptance_failure.hs",
        ),
    );
    let (successor, child) = root_and_child_rounds(&mut rounds, &mut fixture, None).await;
    assert_eq!(successor.request.model, "test-model");
    assert!(has_user_text(&successor.request, "parent-original"));
    assert!(!has_user_text(&successor.request, "must-not-publish"));
    let raw = raw_request_items(&fixture, &successor.request);
    assert_eq!(retained_output_item(&raw, "context-setup"), &original_setup);
    assert_eq!(
        retained_output_item(&successor.request.input, "context-setup"),
        &original_setup
    );
    assert_native_exchange_preserved(&successor.request, &raw, "context-failure");
    assert_eq!(
        successor
            .request
            .input
            .iter()
            .rev()
            .find_map(Item::configuration_effort),
        before
            .history
            .iter()
            .rev()
            .find_map(|(_, _, item)| item.configuration_effort())
    );
    let after = root_context_state(&fixture);
    assert_eq!(after.generation, before.generation);
    assert_eq!(after.model, before.model);
    assert_eq!(
        after
            .history
            .iter()
            .find(|(_, _, item)| item == &original_setup)
            .map(|(_, hash, _)| hash),
        before
            .history
            .iter()
            .find(|(_, _, item)| item == &original_setup)
            .map(|(_, hash, _)| hash)
    );
    let terminal = retained_output(&successor.request, "context-failure");
    assert_eq!(
        terminal["failure"]["phase"],
        tidepool_toolchain::failclass::Phase::Run.tag(),
        "{terminal}"
    );
    assert!(
        terminal
            .to_string()
            .contains("intentional context transaction failure")
    );
    // Context edits roll back, while the admitted actor-owned child and its
    // explicit request survive the enclosing invocation's failure.
    prove_child_survives(
        child,
        &mut rounds,
        &mut fixture,
        "context-failure",
        "context-setup",
        &original_setup,
    )
    .await;
    successor.finish();
    fixture.stop().await.unwrap();
}

#[tokio::test]
async fn resident_async_failure_keeps_bindings_and_activated_child() {
    let (_files, mut fixture, mut rounds) = start().await;
    let actor = fixture.context.actor.identity();
    let setup = next_round(&mut rounds).await;
    setup.async_cell(
        "async-failure-setup",
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/context_acceptance_setup.hs",
        ),
    );
    let parent = next_round_with_output(&mut rounds, "async-failure-setup").await;
    assert!(parent.is_root());
    successful_output(&parent.request, "async-failure-setup");
    let original_setup = retained_output_item(&parent.request.input, "async-failure-setup").clone();
    let session = parent.request.session_id.clone();
    parent.async_cell(
        "async-parent-failure",
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/context_acceptance_async_failure.hs",
        ),
    );

    let (failed, child) = root_and_child_rounds_with_output(
        &mut rounds,
        &mut fixture,
        None,
        Some("async-parent-failure"),
    )
    .await;
    assert_eq!(failed.request.session_id, session);
    assert_eq!(failed.request.model, "test-model");
    let terminal = retained_output(&failed.request, "async-parent-failure");
    assert_eq!(
        terminal["failure"]["phase"],
        tidepool_toolchain::failclass::Phase::Run.tag(),
        "{terminal}"
    );
    assert!(
        terminal
            .to_string()
            .contains("intentional async parent failure"),
        "{terminal}"
    );
    let store = fixture.runtime.store();
    let claims = store
        .claims(&harness::model::CallId("async-parent-failure".into()))
        .unwrap();
    let operation = &claims
        .first()
        .expect("failed call owns its scheduler operation")
        .operation;
    let settled = fixture
        .runtime
        .scheduler()
        .output(operation)
        .await
        .unwrap()
        .expect("the actual failure settles before the successor inference");
    match &settled {
        harness::turn::JobOutput::Completed(Err(failure)) => assert!(
            failure
                .message()
                .contains("intentional async parent failure"),
            "{failure}"
        ),
        _ => panic!("expected the authored native failure, received {settled:?}"),
    }
    prove_child_survives(
        child,
        &mut rounds,
        &mut fixture,
        "async-parent-failure",
        "async-failure-setup",
        &original_setup,
    )
    .await;
    failed.async_cell(
        "async-failure-reuse",
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/context_acceptance_after_failure.hs",
        ),
    );
    let reused = next_round_with_output(&mut rounds, "async-failure-reuse").await;
    assert!(reused.is_root());
    assert_eq!(reused.request.session_id, session);
    let output = successful_output(&reused.request, "async-failure-reuse");
    assert_eq!(test_campaign::explicit_display_text(&output), "42");
    assert_eq!(fixture.context.actor.identity(), actor);
    assert!(fixture.context.actor.terminal().get().is_none());
    reused.finish();
    fixture.stop().await.unwrap();
}

#[tokio::test]
async fn resident_sync_context_cancel_discards_staging_and_keeps_activated_child() {
    let (_files, mut fixture, mut rounds) = start().await;
    let root_identity = fixture.context.actor.identity();
    let setup = next_round(&mut rounds).await;
    setup.cell(
        "context-setup",
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/context_acceptance_setup.hs",
        ),
    );
    let parent = next_round(&mut rounds).await;
    let setup_output = successful_output(&parent.request, "context-setup");
    assert_eq!(
        test_campaign::explicit_display_text(&setup_output),
        "context-setup-retained-result",
        "staging trims the setup cell's genuine native output"
    );
    let original_setup = retained_output_item(&parent.request.input, "context-setup").clone();
    parent.cell_with_reasoning(
        "context-cancel",
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/context_acceptance_cancel.hs",
        ),
    );
    let store = fixture.runtime.store();
    let identity = harness::embedding::HostIdentity {
        run: runtime_namespace(&fixture.context.config.run_directory.path()),
        actor: AgentPath("/root".into()),
        incarnation: fixture.context.actor.identity().incarnation.0.to_string(),
    };
    let mut child = None;
    let waiting = async {
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
                // Store claims can precede scheduler admission. Once retained,
                // completion is observable before terminal Store publication.
                let scheduler = fixture.runtime.scheduler();
                let terminal = match scheduler.invocation_completion(&claim.operation).await {
                    Ok(Some(completion)) => Some(completion.output),
                    Ok(None) | Err(harness::turn::JobError::UnknownCall) => {
                        match scheduler.output(&claim.operation).await {
                            Ok(output) => output,
                            Err(harness::turn::JobError::UnknownCall) => None,
                            Err(error) => {
                                panic!("staged cell terminal observation failed: {error:?}")
                            }
                        }
                    }
                    Err(error) => panic!("staged cell completion observation failed: {error:?}"),
                };
                if let Some(settled) = terminal {
                    panic!(
                        "staged cell settled before its real Sleep wait; operation={:?}; \
                         terminal={settled:?}; graph={:?}",
                        claim.operation,
                        fixture.context.forest.inspect_host_graph()
                    );
                }
                if fixture
                    .context
                    .actor
                    .hosted_workbench_waiting(&context)
                    .is_some()
                {
                    break claim.operation.clone();
                }
            }
            tokio::select! {
                biased;
                round = rounds.recv() => {
                    let round = round.expect("scripted provider closed before the staged Sleep wait");
                    if !round.is_root() {
                        assert!(child.is_none(), "an unanswered child inferred twice");
                        child = Some(round);
                        continue;
                    }
                    let terminal = round.request.input.iter().find(|item| {
                        matches!(item.0["type"].as_str(),
                            Some("custom_tool_call_output" | "function_call_output"))
                            && item.0["call_id"] == "context-cancel"
                    });
                    panic!(
                        "provider inferred before the staged cell reached its real Sleep wait; \
                         session={}; terminal={terminal:?}; settlement={}; graph={:?}",
                        round.request.session_id,
                        fixture.cell_settlement_diagnostic("context-cancel").await,
                        fixture.context.forest.inspect_host_graph()
                    );
                }
                _ = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
        }
    };
    let operation = tokio::time::timeout(
        CELL_TIMEOUT,
        fixture
            .context
            .while_root_live("the staged context-cancel Sleep wait", waiting),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "staged cell never reached its real Sleep wait; graph={:?}",
            fixture.context.forest.inspect_host_graph()
        )
    })
    .expect("exact root exited before the staged cell reached its real Sleep wait");
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
    let (successor, child) = root_and_child_rounds(&mut rounds, &mut fixture, child).await;
    assert_eq!(successor.request.model, "test-model");
    assert!(has_user_text(&successor.request, "parent-original"));
    assert!(!has_user_text(&successor.request, "must-not-publish"));
    let raw = raw_request_items(&fixture, &successor.request);
    assert_eq!(retained_output_item(&raw, "context-setup"), &original_setup);
    assert_eq!(
        retained_output_item(&successor.request.input, "context-setup"),
        &original_setup
    );
    assert_native_exchange_preserved(&successor.request, &raw, "context-cancel");
    assert_eq!(
        successor
            .request
            .input
            .iter()
            .rev()
            .find_map(Item::configuration_effort),
        before
            .history
            .iter()
            .rev()
            .find_map(|(_, _, item)| item.configuration_effort())
    );
    let terminal = retained_output(&successor.request, "context-cancel");
    assert_context_operations_uncommitted(&terminal);
    if let Some(receipt) = terminal.get("receipt") {
        assert_context_operations_uncommitted(receipt);
    }
    let after = root_context_state(&fixture);
    assert_eq!(after.generation, before.generation);
    assert_eq!(after.model, before.model);
    assert_eq!(
        after
            .history
            .iter()
            .find(|(_, _, item)| item == &original_setup)
            .map(|(_, hash, _)| hash),
        before
            .history
            .iter()
            .find(|(_, _, item)| item == &original_setup)
            .map(|(_, hash, _)| hash)
    );
    assert_eq!(fixture.context.actor.identity(), root_identity);
    assert!(fixture.context.actor.terminal().get().is_none());
    let prefix = |history: Vec<(harness::model::RequestId, harness::item::ItemHash, Item)>| {
        history
            .into_iter()
            .filter(|(_, _, item)| item.0["call_id"] != "context-cancel")
            .collect::<Vec<_>>()
    };
    assert_eq!(prefix(after.history), prefix(before.history));
    prove_child_survives(
        child,
        &mut rounds,
        &mut fixture,
        "context-cancel",
        "context-setup",
        &original_setup,
    )
    .await;
    successor.finish();
    fixture.stop().await.unwrap();
}

#[path = "harness_usecase_performance.rs"]
mod harness_usecase_performance;
