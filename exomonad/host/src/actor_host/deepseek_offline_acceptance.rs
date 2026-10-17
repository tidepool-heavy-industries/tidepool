//! Fresh hosted actors using the real admitted HTTP client; only provider replies are scripted.

use super::hosted_test_context::HostedTestRuntime;
use super::run_auth::RunAuth;
use super::test_campaign::{
    actor_output_origin, commit_workspace, configure_command_path, hosted_test_settings,
    retained_committed_display_text, COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
};
use super::*;
use axum::{body::Body, extract::State, response::Response, routing::post, Router};
use harness::embedding::HostIdentity;
use harness::engine::ResponsesTransport;
use harness::model::{CallId, ConversationIdentity, OperationId, RequestId};
use harness::transport::{
    auth::DeepSeekAuth, Auth, CredentialReference, InferenceLimits, NativeTransportAdmission,
    NativeTransportContract, PreparedProviderRequest, PreparedTransportSelection, ResponsesClient,
    ResponsesProtocol, ResponsesRoute, ResponsesTurn, TransportError,
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use tokio::sync::{mpsc, oneshot};

const SPAWN: &str = include_str!("fixtures/deepseek_offline_spawn.hs");
const JOIN: &str = include_str!("fixtures/deepseek_offline_join.hs");
const CLEANUP: &str = include_str!("fixtures/deepseek_offline_cleanup.hs");
const COMMAND_PROOF: &str = include_str!("fixtures/deepseek_offline_command_proof.hs");
const INITIAL: &str = include_str!("fixtures/deepseek_offline_initial_score.rs");
const REPAIRED: &str = include_str!("fixtures/deepseek_offline_repaired_score.rs");
const TESTS: &str = include_str!("fixtures/deepseek_offline_score_tests.rs");
const KEY: &str = "offline-host-synthetic-key-not-a-provider-credential";

/// Observation only: the same opaque prepared value continues into the native
/// client. In particular native admission is forwarded rather than recreated.
struct ObservedClient {
    client: ResponsesClient<RunAuth>,
    issued: mpsc::UnboundedSender<IssuedRequest>,
}

struct IssuedRequest {
    request: RequestId,
    session: String,
    wire: Vec<u8>,
}

impl ObservedClient {
    fn observe(&self, prepared: &PreparedProviderRequest) {
        let _ = self.issued.send(IssuedRequest {
            request: prepared.request_id().clone(),
            session: prepared.request().session_id.thread().to_owned(),
            wire: prepared.wire_body().to_vec(),
        });
    }
}

#[async_trait::async_trait]
impl ResponsesTransport for ObservedClient {
    fn prepare_submission(&self) -> Result<PreparedTransportSelection, TransportError> {
        self.client.prepare_submission()
    }

    fn response_contract(&self) -> Result<(ResponsesRoute, ResponsesProtocol), TransportError> {
        self.client.response_contract()
    }

    async fn create(
        &self,
        prepared: PreparedProviderRequest,
    ) -> Result<ResponsesTurn, TransportError> {
        self.observe(&prepared);
        self.client.create(prepared).await
    }

    async fn create_streaming(
        &self,
        prepared: PreparedProviderRequest,
        sink: mpsc::Sender<harness::transport::sse::StreamEvent>,
    ) -> Result<ResponsesTurn, TransportError> {
        self.observe(&prepared);
        self.client.create_streaming(prepared, sink).await
    }
}

struct HttpRequest {
    bytes: Vec<u8>,
    reply: oneshot::Sender<Vec<u8>>,
}

struct LocalHttp {
    endpoint: String,
    received: mpsc::UnboundedReceiver<HttpRequest>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
}

async fn receive_http(
    State(requests): State<mpsc::UnboundedSender<HttpRequest>>,
    request: axum::extract::Request,
) -> Response {
    let (parts, body) = request.into_parts();
    assert_eq!(parts.method, axum::http::Method::POST);
    assert_eq!(parts.uri.path(), "/responses");
    assert_eq!(
        parts.headers["authorization"].to_str().unwrap(),
        format!("Bearer {KEY}")
    );
    assert!(!parts.headers.contains_key("content-encoding"));
    for header in [
        "chatgpt-account-id",
        "session-id",
        "thread-id",
        "x-client-request-id",
    ] {
        assert!(!parts.headers.contains_key(header));
    }
    let bytes = axum::body::to_bytes(body, 4 * 1024 * 1024)
        .await
        .unwrap()
        .to_vec();
    let (reply, response) = oneshot::channel();
    if requests.send(HttpRequest { bytes, reply }).is_err() {
        return Response::builder().status(503).body(Body::empty()).unwrap();
    }
    match response.await {
        Ok(bytes) => Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(bytes))
            .unwrap(),
        Err(_) => Response::builder().status(503).body(Body::empty()).unwrap(),
    }
}

impl LocalHttp {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/responses", listener.local_addr().unwrap());
        let (requests, received) = mpsc::unbounded_channel();
        let app = Router::new()
            .route("/responses", post(receive_http))
            .with_state(requests);
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
        });
        Self {
            endpoint,
            received,
            stop: Some(stop),
            task: Some(task),
        }
    }

    async fn finish(mut self) {
        assert!(
            self.received.try_recv().is_err(),
            "unexpected extra provider request"
        );
        self.received.close();
        let _ = self.stop.take().unwrap().send(());
        tokio::time::timeout(Duration::from_secs(5), self.task.as_mut().unwrap())
            .await
            .expect("local HTTP connections retire")
            .unwrap()
            .unwrap();
        self.task.take();
    }
}

impl Drop for LocalHttp {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

struct Round {
    actor: ActorRef,
    identity: HostIdentity,
    request: RequestId,
    body: Value,
    reply: oneshot::Sender<Vec<u8>>,
}

impl Round {
    fn operation(&self, call: &str) -> OperationId {
        OperationId {
            origin: ConversationIdentity::Embedded {
                run: self.identity.run.clone(),
                actor: self.identity.actor.clone(),
                incarnation: self.identity.incarnation.clone(),
            },
            request: self.request.clone(),
            call: CallId(call.into()),
        }
    }

    fn output(&self, call: &str) -> Value {
        let matches = self.body["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["type"] == "function_call_output" && item["call_id"] == call)
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "one actual output for {call}");
        serde_json::from_str(matches[0]["output"].as_str().unwrap()).unwrap()
    }

    fn call(self, call: &str, name: &str, arguments: Value) {
        let tool = self.body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .expect("the issuing request advertises the tool");
        assert_eq!(tool["type"], "function");
        // These are the actual advertised wrapper fields, not a parallel tool decoder.
        for field in tool["parameters"]["required"].as_array().unwrap() {
            assert!(
                arguments.get(field.as_str().unwrap()).is_some(),
                "{name}: missing {field}"
            );
        }
        self.respond(
            call,
            vec![json!({"type":"function_call", "id":format!("item-{call}"),
            "status":"completed", "call_id":call, "name":name,
            "arguments":serde_json::to_string(&arguments).unwrap()})],
        );
    }

    fn cell(self, call: &str, source: &str) {
        self.call(call, "haskell_sync", json!({"source":source}));
    }

    fn finish(self, label: &str) {
        self.respond(
            label,
            vec![json!({"type":"message", "id":format!("message-{label}"),
            "status":"completed", "role":"assistant",
            "content":[{"type":"output_text", "text":"offline task complete", "annotations":[]}]})],
        );
    }

    fn respond(self, label: &str, mut items: Vec<Value>) {
        let response_id = format!("response-{}-{label}", self.request.0);
        for item in &mut items {
            item["id"] = json!(format!(
                "{}-{}",
                self.request.0,
                item["id"].as_str().unwrap()
            ));
        }
        let mut events = vec![json!({"type":"response.created", "sequence_number":0,
            "response":{"id":response_id, "object":"response", "status":"in_progress"}})];
        for (index, item) in items.iter().enumerate() {
            events.push(
                json!({"type":"response.output_item.done", "sequence_number":index+1,
                "output_index":index, "item":item}),
            );
        }
        events.push(
            json!({"type":"response.completed", "sequence_number":items.len()+1,
            "response":{"id":response_id, "object":"response", "status":"completed",
                "model":self.body["model"], "output":items,
                "usage":{"input_tokens":10,"output_tokens":10,"total_tokens":20}}}),
        );
        let mut bytes = Vec::new();
        for event in events {
            bytes.extend_from_slice(b"data: ");
            serde_json::to_writer(&mut bytes, &event).unwrap();
            bytes.extend_from_slice(b"\n\n");
        }
        self.reply
            .send(bytes)
            .expect("the production request is still live");
    }
}

struct Driver {
    http: LocalHttp,
    issued: mpsc::UnboundedReceiver<IssuedRequest>,
    pending_http: Vec<HttpRequest>,
    pending_rounds: VecDeque<Round>,
    observed_bodies: HashMap<Vec<u8>, RequestId>,
    observed_requests: HashSet<RequestId>,
    captured_bodies: HashSet<Vec<u8>>,
}

impl Driver {
    async fn next(&mut self, host: &HostedTestRuntime, actor: ActorRef) -> Round {
        if let Some(index) = self
            .pending_rounds
            .iter()
            .position(|round| round.actor == actor)
        {
            return self.pending_rounds.remove(index).unwrap();
        }
        loop {
            let issued = host
                .while_host_running(tokio::time::timeout(
                    COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
                    self.issued.recv(),
                ))
                .await
                .unwrap()
                .expect("actual native request within existing cold settlement budget")
                .expect("native observation remains open");
            assert!(
                self.observed_requests.insert(issued.request.clone()),
                "a prepared request is forwarded only once"
            );
            assert!(
                self.observed_bodies
                    .insert(issued.wire.clone(), issued.request.clone())
                    .is_none(),
                "this bounded script requires a unique wire body per actual issued request"
            );
            let http = loop {
                let matches = self
                    .pending_http
                    .iter()
                    .enumerate()
                    .filter_map(|(index, http)| (http.bytes == issued.wire).then_some(index))
                    .collect::<Vec<_>>();
                assert!(matches.len() <= 1, "ambiguous captured HTTP body");
                if let Some(index) = matches.first() {
                    break self.pending_http.remove(*index);
                }
                let request = host
                    .while_host_running(tokio::time::timeout(
                        Duration::from_secs(15),
                        self.http.received.recv(),
                    ))
                    .await
                    .unwrap()
                    .expect("issued native client reaches the local listener")
                    .expect("HTTP fixture remains open");
                assert!(
                    self.captured_bodies.insert(request.bytes.clone()),
                    "a second byte-identical HTTP request cannot select an issued identity"
                );
                self.pending_http.push(request);
            };
            let graph = host.context.forest.inspect_host_graph().await.unwrap();
            let mut matches = graph.iter().filter_map(|node| {
                let binding = host.context.binding(node.actor)?;
                let conversation = binding.conversation()?;
                let identity = conversation.identity();
                let session = format!(
                    "{}:{}:{}",
                    identity.run, identity.actor.0, identity.incarnation
                );
                (session == issued.session).then(|| (node.actor, identity.clone()))
            });
            let (actual_actor, identity) = matches
                .next()
                .expect("issued session belongs to actual actor binding");
            assert!(matches.next().is_none(), "session ownership is unambiguous");
            assert_eq!(
                host.runtime
                    .store()
                    .embedded_round_frontier(&identity)
                    .unwrap()
                    .pending_head,
                Some(issued.request.clone())
            );
            assert!(host
                .runtime
                .store()
                .request(&issued.request)
                .unwrap()
                .is_some());
            let body: Value = serde_json::from_slice(&http.bytes).unwrap();
            assert_eq!(body["stream"], true);
            assert_eq!(body["max_output_tokens"], 65_536);
            assert!(body.get("previous_response_id").is_none());
            let round = Round {
                actor: actual_actor,
                identity,
                request: issued.request,
                body,
                reply: http.reply,
            };
            if actual_actor == actor {
                return round;
            }
            self.pending_rounds.push_back(round);
        }
    }
}

fn canonical(
    host: &HostedTestRuntime,
    issuer: &HostIdentity,
    operation: &OperationId,
    next: &Round,
) -> Value {
    let recorded = host
        .runtime
        .store()
        .inspect_embedded_result(issuer, operation)
        .unwrap()
        .expect("exact issuing operation retains canonical settlement");
    assert_eq!(recorded.terminal, harness::store::TerminalOutcome::Success);
    assert_eq!(
        recorded.output_format,
        harness::provider::OutputFormat::NativeReceiptV1
    );
    let projected = super::embedded_policy::project_retained_result(&recorded, operation)
        .unwrap()
        .unwrap();
    assert_eq!(next.output(&operation.call.0), projected);
    let claims = host
        .runtime
        .store()
        .claims_for_operation(operation)
        .unwrap();
    assert_eq!(claims.len(), 1, "one exact claim for the original effect");
    assert_eq!(claims[0].state, harness::store::ClaimState::Settled);
    let envelope: Value =
        serde_json::from_str(recorded.item.0["output"].as_str().unwrap()).unwrap();
    assert_eq!(envelope["nativeResult"]["kind"], "workbench");
    let body = envelope["nativeResult"]["body"].clone();
    assert_eq!(body["publication"]["status"], "published", "{body}");
    body
}

fn display_true(host: &HostedTestRuntime, previous: &(HostIdentity, OperationId), next: &Round) {
    let body = canonical(host, &previous.0, &previous.1, next);
    assert_eq!(
        retained_committed_display_text(
            &host.runtime.store(),
            &actor_output_origin(host.runtime.run_identity(), next.actor),
            &body
        ),
        "True"
    );
}

fn command_success(
    host: &HostedTestRuntime,
    previous: &(HostIdentity, OperationId),
    next: &Round,
) -> String {
    let body = canonical(host, &previous.0, &previous.1, next);
    assert_eq!(body["status"], "committed");
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(item["status"], "committed");
    assert_eq!(item["presentation"]["status"], "published");
    let bindings = item["installedBindings"].as_array().unwrap();
    assert_eq!(bindings.len(), 1);
    let binding = bindings[0].as_str().unwrap();
    assert_eq!(body["publication"]["bindings"], json!([binding]));
    let facts = &item["value"];
    assert_eq!(facts["state"], "Finished", "{body}");
    assert_eq!(facts["successful"], true, "{body}");
    assert_eq!(
        facts["outcome"],
        json!({"tag":"OutcomeExited","contents":0}),
        "{body}"
    );
    assert_eq!(facts["cleanup"], "CleanupClean", "{body}");
    assert_eq!(facts["retained_binding"], binding);
    binding.to_owned()
}

fn issued_call(round: &Round, call: &str) -> (HostIdentity, OperationId) {
    (round.identity.clone(), round.operation(call))
}

fn shell_word(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn bash(round: Round, call: &str, cmd: String) {
    round.call(
        call,
        "bash",
        json!({"arguments":{"cmd":cmd, "workdir":null, "environment":null,
        "memory_mib":null, "tty":null, "stdin":null, "yield_time_ms":30000,
        "max_output_bytes":4096, "focus":null, "background":false,
        "intent":"offline hosted Rust fixture compile and verification"},"delivery":"wait"}),
    );
}

fn configured_workspace(config: &mut ActorHostConfig, auth: DeepSeekAuth) {
    super::scaffold_admission_tests::prepared_scaffold(config);
    configure_command_path(config);
    config.model = exomonad_actor::Model::DeepSeekFlash;
    config.reasoning = exomonad_actor::Reasoning::DeepSeekReasoning(
        exomonad_actor::DeepSeekReasoning::DeepSeekHigh,
    );
    config.inference_auth = Some(RunAuth::DeepSeek(auth));
    config.jev = Some(Arc::new(super::test_campaign::FixtureJev));
    crate::exomonad::edit_fixture_project_config(&config.workspace.join(".exomonad"), |project| {
        project.defaults.model = config.model.clone();
        project.defaults.reasoning = config.reasoning.clone();
    });
    std::fs::write(config.workspace.join("score.rs"), INITIAL).unwrap();
    commit_workspace(&config.workspace);
    config.workspace_inputs = Some(
        crate::exomonad::workspace::FrozenWorkspace::load(
            &config.workspace,
            config.run_directory.path(),
        )
        .unwrap(),
    );
}

#[tokio::test]
#[ignore = "requires production hosted compiler, delegated command resources and declared Rust toolchain"]
async fn hosted_deepseek_edits_tests_and_joins_two_workers() {
    let rustc = std::env::var("EXOMONAD_OFFLINE_RUSTC").expect("declared pinned rustc resource");
    let cc = std::env::var("EXOMONAD_OFFLINE_CC").expect("declared pinned linker resource");
    assert!(std::path::Path::new(&rustc).is_absolute());
    assert!(std::path::Path::new(&cc).is_absolute());
    let files = tempfile::tempdir().unwrap();
    let http = LocalHttp::start().await;
    let key_path = files.path().join("synthetic-deepseek-key");
    let mut key = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&key_path)
        .unwrap();
    writeln!(key, "{KEY}").unwrap();
    let admission = NativeTransportAdmission::rehydrate(NativeTransportContract {
        route: ResponsesRoute::DeepSeek,
        protocol: ResponsesProtocol::Standard,
        endpoint: http.endpoint.clone(),
        credential: CredentialReference::File {
            path: key_path.clone(),
        },
        limits: InferenceLimits::deepseek_default(),
    })
    .unwrap();
    let auth = DeepSeekAuth::from_admission(admission).unwrap();
    let expected_contract = auth.admission().unwrap().contract().clone();
    let mut settings = hosted_test_settings(&files, 3);
    settings.provider = exomonad_actor::Backend::DeepSeek;
    settings.credential_file = key_path;
    assert_eq!(
        settings.inference_limits.resolve(settings.provider),
        auth.admission().unwrap().contract().limits
    );
    let (issued, received) = mpsc::unbounded_channel();
    let host = HostedTestRuntime::start_with_factory(
        &settings,
        Some(exomonad_node::command_resources::CommandResourcePolicy {
            general_bytes: 2 * exomonad_node::command_resources::GIB,
            protected_bytes: 0,
            swap_max_bytes: 0,
            ..Default::default()
        }),
        |config| configured_workspace(config, auth),
        move |runtime, _| {
            assert_eq!(runtime.inference_auth().admission().unwrap().contract(), &expected_contract,
                "the fresh host must retain the exact synthetic local endpoint and credential source before any HTTP client dispatch");
            Arc::new(ObservedClient {
                client: ResponsesClient::new(runtime.inference_auth().clone()),
                issued,
            })
        },
    )
    .await
    .expect("fresh production host binds the synthetic admitted DeepSeek run before dispatch");
    let mut driver = Driver {
        http,
        issued: received,
        pending_http: vec![],
        pending_rounds: VecDeque::new(),
        observed_bodies: HashMap::new(),
        observed_requests: HashSet::new(),
        captured_bodies: HashSet::new(),
    };
    host.run_scenario(|host| Box::pin(async move {
        let root_actor = host.context.actor.identity();
        host.http_input("deepseek-offline-hosted: repair and test the temporary Rust repository").await.unwrap();
        let root = driver.next(host, root_actor).await;
        let parent_selection = (root.body["model"].clone(), root.body["reasoning"].clone());
        assert_eq!(parent_selection, (json!("deepseek-flash"), json!({"effort":"high"})));
        let spawn = issued_call(&root, "offline-spawn");
        root.cell("offline-spawn", SPAWN);
        let root = driver.next(host, root_actor).await;
        display_true(host, &spawn, &root);
        let graph = host.context.forest.inspect_host_graph().await.unwrap();
        let children = graph.iter().filter(|node| node.creator == Some(root_actor)).collect::<Vec<_>>();
        assert_eq!(children.len(), 2);
        let actor_named = |label: &str| {
            let matching = children.iter().filter(|node| node.label == label).collect::<Vec<_>>();
            assert_eq!(matching.len(), 1);
            let node = matching[0];
            assert_eq!(node.supervisor_parent, Some(root_actor));
            assert_eq!(node.context_parent, None);
            assert!(node.model_actor);
            assert!(node.bound_worktree.is_some());
            node.actor
        };
        let editor = actor_named("offline-editor");
        let tester = actor_named("offline-tester");
        assert_ne!(editor, tester);
        let workspace_of = |actor| graph.iter().find(|node| node.actor == actor).unwrap().bound_worktree.as_ref().unwrap();
        assert_eq!(workspace_of(editor), workspace_of(tester));
        let storage = actor_worktree_storage_root(&host.context.config.workspace,
            host.context.config.run_directory.path()).unwrap();
        let storage = tidepool_atomic_write::DirectoryAnchor::open_existing(storage).unwrap();
        let registry = exomonad_worktree::WorktreeRegistry::open(&storage, "registry").unwrap();
        let workspace = registry.get(&exomonad_worktree::WorktreeId::from_raw(workspace_of(editor).clone()))
            .unwrap().expect("the actual SameDir capability retains its workspace owner");
        assert_eq!(workspace.origin, exomonad_worktree::WorktreeOrigin::CurrentRepository);
        assert_eq!(workspace.cwd, std::fs::canonicalize(&host.context.config.workspace).unwrap());
        assert_eq!(workspace.source_repository, workspace.cwd);
        let join = issued_call(&root, "offline-join");
        root.cell("offline-join", JOIN);
        // Both actors reach actual HTTP before either receives a scripted answer.
        let editor_round = driver.next(host, editor).await;
        let tester_round = driver.next(host, tester).await;
        assert_eq!(editor_round.body["model"], "deepseek-pro");
        assert_eq!(editor_round.body["reasoning"], json!({"effort":"high"}));
        assert_eq!((tester_round.body["model"].clone(), tester_round.body["reasoning"].clone()), parent_selection);
        let mut commands = Vec::new();
        for (actor, round, call, path, contents, answer) in [
            (editor, editor_round, "offline-edit-file", "score.rs", REPAIRED, "edited"),
            (tester, tester_round, "offline-test-file", "score_tests.rs", TESTS, "tests-written"),
        ] {
            let command = issued_call(&round, call);
            bash(round, call, format!("printf %s {} > {}", shell_word(contents), shell_word(path)));
            commands.push((actor, command, path, contents, answer));
        }
        let mut replies = Vec::new();
        for (actor, command, path, contents, answer) in commands {
            let round = host.while_operation_succeeds(&command.1, driver.next(host, actor)).await
                .expect("the actual command succeeds before the next inference");
            command_success(host, &command, &round);
            assert_eq!(std::fs::read_to_string(host.context.config.workspace.join(path)).unwrap(), contents);
            let reply = issued_call(&round, "offline-worker-reply");
            round.cell("offline-worker-reply", &format!("respond ({answer:?} :: Text)"));
            replies.push((actor, reply));
        }
        for (actor, reply) in replies {
            let round = driver.next(host, actor).await;
            let body = canonical(host, &reply.0, &reply.1, &round);
            assert_eq!(body["status"], "replied");
            assert_eq!(body["items"].as_array().unwrap().last().unwrap()["terminalTransfer"], "replyAccepted");
            round.finish(&format!("offline-worker-{}", actor.id.0));
        }
        let root = driver.next(host, root_actor).await;
        display_true(host, &join, &root);
        let compile = issued_call(&root, "offline-rust-tests");
        bash(root, "offline-rust-tests", format!("{} --edition=2021 --test -C linker={} score_tests.rs -o score-tests && ./score-tests --test-threads=1",
            shell_word(&rustc), shell_word(&cc)));
        let root = host.while_operation_succeeds(&compile.1, driver.next(host, root_actor)).await
            .expect("the actual rustc and test process succeed before the next inference");
        let command_binding = command_success(host, &compile, &root);
        let proof = issued_call(&root, "offline-command-proof");
        assert_eq!(COMMAND_PROOF.matches("__OFFLINE_RETAINED_JOB__").count(), 1);
        root.cell("offline-command-proof", &COMMAND_PROOF.replace("__OFFLINE_RETAINED_JOB__", &command_binding));
        let root = driver.next(host, root_actor).await;
        display_true(host, &proof, &root);
        let cleanup = issued_call(&root, "offline-workers-stop");
        root.cell("offline-workers-stop", CLEANUP);
        let root = driver.next(host, root_actor).await;
        display_true(host, &cleanup, &root);
        for actor in [editor, tester] {
            let installed = host.context.observer.installation(actor).await;
            let terminal = installed.actor.terminal();
            tokio::time::timeout(Duration::from_secs(30), terminal.wait()).await.expect("explicit stop retires exact worker");
            let cleanup = terminal.cleanup().expect("worker retirement retains cleanup evidence");
            assert_eq!(cleanup.actor(), actor);
            assert!(cleanup.is_confirmed(), "{cleanup:?}");
            host.context.config.command_resources.as_ref().unwrap().seal_producer(
                &exomonad_node::command_resources::CommandActorIdentity::new(
                    actor.id.0, actor.incarnation.0)).await
                .expect("the exact retired worker has no active commands or retained allocations");
        }
        assert!(driver.pending_http.is_empty());
        assert!(driver.pending_rounds.is_empty());
        assert_eq!(driver.observed_bodies.len(), driver.captured_bodies.len());
        root.finish("offline-hosted-final");
        driver.http.finish().await;
    })).await;
}
