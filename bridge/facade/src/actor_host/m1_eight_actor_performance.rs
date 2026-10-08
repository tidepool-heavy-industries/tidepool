//! Eight real captured child actors, sequential cells within each actor.

use super::warm_cell_performance::{
    require_owned_daemon, ClientRequests, CompilerInvocation, DaemonTrace, Workload,
};
use super::*;
use harness::model::{CallId, ConversationIdentity, OperationId, RequestId};
use std::collections::{BTreeSet, HashSet};
use std::time::Instant;
use tracing_subscriber::prelude::*;

const ACTORS: usize = 8;
const PER_ACTOR: usize = 10;

struct ActorCell {
    actor: usize,
    sequence: usize,
    operation: OperationId,
    started: Instant,
}
struct Display {
    cell: ActorCell,
    elapsed_ns: u128,
    release: tokio::sync::oneshot::Sender<()>,
}
#[derive(Default)]
struct ChildState {
    ordinal: usize,
    next: usize,
    pending: Option<ActorCell>,
}
struct EightActorTransport {
    clients: ClientRequests,
    workloads: Vec<Workload>,
    children: Mutex<HashMap<ConversationIdentity, ChildState>>,
    ready: watch::Sender<bool>,
    root_round: AtomicUsize,
    displays: mpsc::UnboundedSender<Display>,
    root_finished: tokio::sync::Notify,
}

fn final_item(text: &str) -> harness::item::Item {
    harness::item::Item(
        json!({"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":text}]}),
    )
}

impl EightActorTransport {
    async fn turn(&self, request_id: &RequestId, request: ResponsesRequest) -> ResponsesTurn {
        let (prefix, incarnation) = request.session_id.rsplit_once(':').unwrap();
        let (run, path) = prefix.rsplit_once(':').unwrap();
        let origin = ConversationIdentity::Embedded {
            run: run.into(),
            actor: AgentPath(path.into()),
            incarnation: incarnation.into(),
        };
        let operation = |call: &str| OperationId {
            origin: origin.clone(),
            request: request_id.clone(),
            call: CallId(call.into()),
        };
        let item = if path == "/root" {
            match self.root_round.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    self.clients.issue(&operation("eight-actor-setup"));
                    harness::item::Item(
                        json!({"type":"custom_tool_call","call_id":"eight-actor-setup","name":"haskell","input":format!("{}\n_ <- display True", tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_later_failure_scope_setup.hs"))}),
                    )
                }
                1 => {
                    assert!(request.input.iter().any(|item| {
                        if item.0["type"] != "custom_tool_call_output"
                            || item.0["call_id"] != "eight-actor-setup"
                        {
                            return false;
                        }
                        let Some(output) = item.0["output"].as_str() else {
                            return false;
                        };
                        let value: Value = serde_json::from_str(output).unwrap();
                        matches!(value["status"].as_str(), Some("completed" | "committed"))
                            && value["items"]
                                .as_array()
                                .unwrap()
                                .last()
                                .is_some_and(|item| {
                                    item["output"]
                                        .as_str()
                                        .is_some_and(|output| output.trim() == "True")
                                })
                    }));
                    self.clients.issue(&operation("eight-actor-launch"));
                    harness::item::Item(
                        json!({"type":"custom_tool_call","call_id":"eight-actor-launch","name":"haskell","input":tidepool_testing::fixture_source("bridge/facade/src/actor_host/m1_eight_actor_launch.hs")}),
                    )
                }
                2 => {
                    assert!(request.input.iter().any(|item| cell_output_matches(
                        item,
                        "eight-actor-launch",
                        "True"
                    )));
                    self.root_finished.notify_one();
                    final_item(
                        "All eight typed replies arrived through their original parent invocation.",
                    )
                }
                _ => panic!("unexpected supervisor model request"),
            }
        } else {
            let display = {
                let mut children = self.children.lock();
                let ordinal = children.len();
                let child = children
                    .entry(origin.clone())
                    .or_insert_with(|| ChildState {
                        ordinal,
                        ..Default::default()
                    });
                assert!(child.ordinal < ACTORS, "only eight measured child actors");
                if let Some(cell) = child.pending.take() {
                    let workload = &self.workloads[cell.sequence];
                    assert!(
                        request.input.iter().any(|item| cell_output_matches(
                            item,
                            &cell.operation.call.0,
                            &workload.expected
                        )),
                        "exact source display before next cell"
                    );
                    let elapsed_ns = cell.started.elapsed().as_nanos();
                    Some((cell, elapsed_ns))
                } else {
                    None
                }
            };
            if let Some((cell, elapsed_ns)) = display {
                let (release, released) = tokio::sync::oneshot::channel();
                self.displays
                    .send(Display {
                        cell,
                        elapsed_ns,
                        release,
                    })
                    .unwrap();
                released
                    .await
                    .expect("measurement owner releases this actor's next sequential cell");
            }
            {
                let children = self.children.lock();
                if children.len() == ACTORS {
                    self.ready.send_replace(true);
                }
            }
            let mut ready = self.ready.subscribe();
            while !*ready.borrow_and_update() {
                ready.changed().await.unwrap();
            }
            let mut children = self.children.lock();
            let child = children.get_mut(&origin).unwrap();
            let sequence = child.next;
            child.next += 1;
            if sequence <= PER_ACTOR {
                // Deliberately reuse these provider IDs across actors.
                let call = format!("eight-actor-cell-{sequence}");
                let operation = operation(&call);
                self.clients.issue(&operation);
                child.pending = Some(ActorCell {
                    actor: child.ordinal,
                    sequence,
                    operation,
                    started: Instant::now(),
                });
                harness::item::Item(
                    json!({"type":"custom_tool_call","call_id":call,"name":"haskell","input":self.workloads[sequence].source}),
                )
            } else if sequence == PER_ACTOR + 1 {
                self.clients.issue(&operation("eight-actor-reply"));
                harness::item::Item(
                    json!({"type":"custom_tool_call","call_id":"eight-actor-reply","name":"haskell","input":"respond (42 :: Int)"}),
                )
            } else {
                final_item("Measured cells and typed reply complete.")
            }
        };
        ResponsesTurn {
            response_id: format!("eight-actor-{}", uuid::Uuid::new_v4()),
            items: vec![item],
            usage: Usage::default(),
        }
    }
}

#[async_trait]
impl ResponsesTransport for EightActorTransport {
    async fn create(&self, _: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        panic!("durable original request identity required")
    }
    async fn create_streaming_for_request(
        &self,
        request_id: &RequestId,
        request: ResponsesRequest,
        sink: mpsc::Sender<harness::transport::sse::StreamEvent>,
    ) -> Result<ResponsesTurn, TransportError> {
        let turn = self.turn(request_id, request).await;
        for item in &turn.items {
            let _ = sink
                .send(harness::transport::sse::StreamEvent::ItemDone(item.clone()))
                .await;
        }
        Ok(turn)
    }
}

#[tokio::test]
#[ignore = "requires exclusive owned matched measurement daemon"]
async fn production_engine_store_eight_actors_sequential_cells() {
    let trace_path = std::path::PathBuf::from(
        std::env::var_os("TIDEPOOL_PERFORMANCE_COMPILER_TRACE").expect("owned daemon trace"),
    );
    assert!(trace_path.is_absolute());
    let socket = std::path::PathBuf::from(
        std::env::var_os(tidepool_extract_cmd::DAEMON_SOCKET_ENV).unwrap(),
    );
    let endpoint = tidepool_extract_cmd::preflight_compiler_daemon(&socket).unwrap();
    let mut trace = DaemonTrace::open(&trace_path);
    let boots = trace
        .read()
        .into_iter()
        .filter(|row| row["message"] == "compiler daemon ready")
        .collect::<Vec<_>>();
    assert_eq!(boots.len(), 1);
    require_owned_daemon(&boots[0]);
    assert_eq!(boots[0]["producer"], endpoint.producer_hex());
    let epoch = boots[0]["daemon_epoch"].as_str().unwrap();
    let clients = ClientRequests::default();
    tracing_subscriber::registry()
        .with(clients.clone())
        .try_init()
        .expect("isolated measurement subscriber");
    let (displays, mut observations) = mpsc::unbounded_channel();
    let (ready, _) = watch::channel(false);
    let transport = Arc::new(EightActorTransport {
        clients: clients.clone(),
        workloads: serde_json::from_str(include_str!("m1_warm_cell_workloads.json")).unwrap(),
        children: Mutex::new(HashMap::new()),
        ready,
        root_round: AtomicUsize::new(0),
        displays,
        root_finished: tokio::sync::Notify::new(),
    });
    let files = tempfile::tempdir().unwrap();
    let assets = files.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
    let secret_file = files.path().join("secret");
    let secret = "offline-eight-actor-measurement-long-secret";
    std::fs::write(&secret_file, secret).unwrap();
    let auth_file = files.path().join("auth.json");
    std::fs::write(&auth_file, "{}").unwrap();
    let settings = EmbeddedLaunchConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Https,
        public_origin: None,
        asset_root: assets,
        browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret,
        session_secret_file: Some(secret_file),
        provider: crate::exomonad::EmbeddedModelProvider::Codex,
        credential_file: auth_file,
        context_capacity_tokens: 2_000_000,
        concurrent_jobs: ACTORS + 1,
    };
    let provider: Arc<dyn ResponsesTransport> = transport.clone();
    let fixture = HostedTestRuntime::start(&settings, &provider)
        .await
        .unwrap();
    trace.read();
    let api = format!("http://{}/api", fixture.address);
    let origin = format!("https://{}", fixture.address);
    let client = reqwest::Client::new();
    let login = client
        .post(format!("{api}/session"))
        .header("Origin", &origin)
        .json(&json!({"secret":secret}))
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
    let accepted = client
        .post(format!("{api}/commands"))
        .header("Origin", &origin)
        .header(reqwest::header::COOKIE, cookie)
        .json(&browser_input(
            &browser_target(&fixture.context),
            "Launch eight real captured measurement actors.",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), reqwest::StatusCode::ACCEPTED);
    let mut measured = Vec::new();
    let mut next = [0; ACTORS];
    for sequence in 0..=PER_ACTOR {
        let mut wave = Vec::new();
        let mut actors = HashSet::new();
        for _ in 0..ACTORS {
            let observed = tokio::time::timeout(Duration::from_secs(300), observations.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(observed.cell.sequence, sequence);
            assert!(actors.insert(observed.cell.actor));
            wave.push(observed);
        }
        // All children are held at their actual provider display boundary.
        // Snapshot gauges belong to the exact machine owner, never a global delta.
        let nodes = fixture.context.forest.inspect_host_graph();
        let target = browser_target(&fixture.context);
        let conversations = HashMap::from([(fixture.context.actor.identity(), target.clone())]);
        let mut releases = Vec::new();
        for observed in wave {
            assert_eq!(observed.cell.sequence, next[observed.cell.actor]);
            next[observed.cell.actor] += 1;
            let operation = &observed.cell.operation;
            let store = fixture.runtime.store();
            let claims = store.claims_for_operation(operation).unwrap();
            assert_eq!(claims.len(), 1);
            assert_eq!(claims[0].state, harness::store::ClaimState::Settled);
            assert!(store
                .replay_tool_output_operation(operation)
                .unwrap()
                .is_some());
            let matching = nodes.iter().filter(|node| {
                if node.actor == fixture.context.actor.identity() { return false; }
                let parent = embedded_context::selected_provider_parent(&target.run, node.creator, &nodes, &conversations).unwrap();
                parent.child_path(node.actor) == *operation.origin.actor()
                    && matches!(&operation.origin, ConversationIdentity::Embedded { incarnation, .. } if incarnation == &node.actor.incarnation.0.to_string())
            }).collect::<Vec<_>>();
            assert_eq!(
                matching.len(),
                1,
                "exact measured actor incarnation is in the existing host graph"
            );
            let actor = matching[0].actor;
            let session = fixture
                .context
                .forest
                .actor_session(actor)
                .expect("exact child session placement");
            let machine = fixture
                .context
                .forest
                .measurement_snapshot_for(actor)
                .expect("paused measured actor has an idle native owner snapshot");
            if observed.cell.sequence > 0 {
                let correlations = clients.requests(operation);
                assert!(!correlations.is_empty());
                let workload = &transport.workloads[observed.cell.sequence];
                measured.push(json!({"schema":1,"composition":"engine-store-eight-actor","runner_id":"actor_workload","actor_index":observed.cell.actor,"sequence":observed.cell.sequence - 1,"operation_id":operation,"elapsed_ns":observed.elapsed_ns,"completed":true,"displayed":true,"workload":workload.workload,"source":workload.source,"source_digest":workload.source_digest,"daemon_epoch":epoch,"compiler_requests":correlations,"machine_owner":{"actor":actor,"session":session.0},"machine":machine,"counter_scope":"exact-session-gauges-at-display-wave"}));
            }
            releases.push(observed.release);
        }
        for release in releases {
            release.send(()).unwrap();
        }
    }
    tokio::time::timeout(Duration::from_secs(300), transport.root_finished.notified())
        .await
        .expect("all eight real typed replies before parent return");
    assert_eq!(transport.children.lock().len(), ACTORS);
    assert_eq!(next, [PER_ACTOR + 1; ACTORS]);
    let expected = measured
        .iter()
        .flat_map(|row| row["compiler_requests"].as_array().unwrap())
        .map(CompilerInvocation::from_trace)
        .collect::<BTreeSet<_>>();
    let mut completed = HashSet::new();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            for row in trace.read() {
                if row["message"] == "compiler request finished"
                    && expected.contains(&CompilerInvocation::from_trace(&row))
                {
                    assert_eq!(row["daemon_epoch"], epoch);
                    assert_eq!(row["daemon_pid"], boots[0]["daemon_pid"]);
                    assert_eq!(row["exit_code"], 0);
                    assert_eq!(row["transport"], "daemon");
                    assert!(row["worker_pid"].as_u64().is_some_and(|pid| pid > 0));
                    assert!(completed.insert(CompilerInvocation::from_trace(&row)));
                }
            }
            if completed.len() == expected.len() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("every exact measured operation joins a real compiler completion");
    fixture
        .stop()
        .await
        .expect("supervisor and all child owners acknowledge cleanup");
    for mut row in measured {
        row["typed_reply_confirmed"] = json!(true);
        row["owner_cleanup_confirmed"] = json!(true);
        eprintln!("actor-performance {row}");
    }
}
