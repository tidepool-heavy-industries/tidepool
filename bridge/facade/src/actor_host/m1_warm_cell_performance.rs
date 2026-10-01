//! Actual production HTTP/Engine/Store warm-cell measurement.

use super::*;
use harness::model::{CallId, ConversationIdentity, OperationId, RequestId};
use std::collections::{BTreeSet, HashSet};
use std::io::{BufRead, BufReader, Seek};
use std::time::Instant;
use tokio::sync::oneshot;
use tracing_subscriber::prelude::*;

const WARM_UP: usize = 10;
const MEASURED: usize = 50;

#[derive(Clone, serde::Deserialize)]
struct Workload {
    workload: String,
    source: String,
    expected: String,
    source_digest: String,
}

#[derive(Clone, Default)]
struct ClientRequests(Arc<Mutex<Vec<String>>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for ClientRequests {
    fn on_new_span(
        &self,
        attributes: &tracing::span::Attributes<'_>,
        _id: &tracing::Id,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if attributes.metadata().name() != "compile_request"
            || attributes.metadata().target() != "tidepool_extract_cmd::endpoint"
        {
            return;
        }
        struct Correlation(Option<String>);
        impl tracing::field::Visit for Correlation {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "compile_request" {
                    self.0 = Some(format!("{value:?}"));
                }
            }
        }
        let mut correlation = Correlation(None);
        attributes.record(&mut correlation);
        let mut requests = self.0.lock();
        assert!(
            requests.len() < 10_000,
            "bounded compiler observation budget"
        );
        requests.push(correlation.0.expect("existing client request digest"));
    }
}

struct PendingCell {
    sequence: usize,
    operation: OperationId,
    started: Instant,
    client_cursor: usize,
}

struct DisplayedCell {
    pending: PendingCell,
    elapsed_ns: u128,
    successor: ResponsesRequest,
    release: oneshot::Sender<()>,
}

struct WarmTransport {
    workloads: Vec<Workload>,
    clients: ClientRequests,
    pending: Mutex<Option<PendingCell>>,
    next: AtomicUsize,
    displayed: mpsc::UnboundedSender<DisplayedCell>,
}

impl WarmTransport {
    async fn turn(
        &self,
        request_id: &RequestId,
        request: ResponsesRequest,
    ) -> Result<ResponsesTurn, TransportError> {
        let completed = {
            let mut pending = self.pending.lock();
            if let Some(cell) = pending.as_ref() {
                let call_id = &cell.operation.call.0;
                let workload = &self.workloads[cell.sequence];
                if let Some(output) = request.input.iter().find(|item| {
                    item.0["type"] == "custom_tool_call_output" && item.0["call_id"] == *call_id
                }) {
                    let displayed = Instant::now();
                    assert!(
                        cell_output_matches(output, call_id, &workload.expected),
                        "source-backed display mismatch for {}: {:?}",
                        workload.source,
                        output
                    );
                    Some((pending.take().unwrap(), displayed))
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some((pending, displayed)) = completed {
            let elapsed_ns = displayed.duration_since(pending.started).as_nanos();
            let (release, released) = oneshot::channel();
            self.displayed
                .send(DisplayedCell {
                    pending,
                    elapsed_ns,
                    successor: request.clone(),
                    release,
                })
                .unwrap();
            released
                .await
                .expect("measurement checks release the next real cell");
        }
        let item = if self.pending.lock().is_some() {
            final_message("Waiting for the actual resident display.")
        } else {
            let sequence = self.next.fetch_add(1, Ordering::SeqCst);
            if sequence >= WARM_UP + MEASURED {
                final_message("All actual warm-cell displays were checked.")
            } else {
                let workload = &self.workloads[sequence];
                let (prefix, incarnation) = request.session_id.rsplit_once(':').unwrap();
                let (run, actor) = prefix.rsplit_once(':').unwrap();
                let call_id = format!("warm-resident-cell-{sequence}");
                let operation = OperationId {
                    origin: ConversationIdentity::Embedded {
                        run: run.into(),
                        actor: AgentPath(actor.into()),
                        incarnation: incarnation.into(),
                    },
                    request: request_id.clone(),
                    call: CallId(call_id.clone()),
                };
                *self.pending.lock() = Some(PendingCell {
                    sequence,
                    operation,
                    client_cursor: self.clients.0.lock().len(),
                    started: Instant::now(),
                });
                harness::item::Item(
                    json!({"type":"custom_tool_call", "name":"haskell", "call_id":call_id, "input":workload.source}),
                )
            }
        };
        Ok(ResponsesTurn {
            response_id: format!("warm-production-response-{}", uuid::Uuid::new_v4()),
            items: vec![item],
            usage: Usage::default(),
        })
    }
}

fn final_message(text: &str) -> harness::item::Item {
    harness::item::Item(
        json!({"type":"message", "role":"assistant", "phase":"final_answer", "content":[{"type":"output_text", "text":text}]}),
    )
}

#[async_trait]
impl ResponsesTransport for WarmTransport {
    async fn create(&self, _: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        panic!("performance fixture requires the Engine's durable request identity")
    }

    async fn create_streaming_for_request(
        &self,
        request_id: &RequestId,
        request: ResponsesRequest,
        sink: mpsc::Sender<harness::transport::sse::StreamEvent>,
    ) -> Result<ResponsesTurn, TransportError> {
        let turn = self.turn(request_id, request).await?;
        for item in &turn.items {
            let _ = sink
                .send(harness::transport::sse::StreamEvent::ItemDone(item.clone()))
                .await;
        }
        Ok(turn)
    }
}

// The cursor reads each trace byte once, retaining an unfinished trailing line.
pub(super) struct DaemonTrace {
    input: BufReader<std::fs::File>,
    partial: String,
}

impl DaemonTrace {
    pub(super) fn open(path: &std::path::Path) -> Self {
        Self {
            input: BufReader::new(std::fs::File::open(path).expect("retained owned-daemon JSONL")),
            partial: String::new(),
        }
    }

    pub(super) fn read(&mut self) -> Vec<Value> {
        let mut rows = Vec::new();
        loop {
            let mut line = String::new();
            if self.input.read_line(&mut line).unwrap() == 0 {
                break;
            }
            self.partial.push_str(&line);
            if !self.partial.ends_with('\n') {
                break;
            }
            let event: Value =
                serde_json::from_str(&self.partial).expect("complete actual daemon trace row");
            self.partial.clear();
            let mut fields = serde_json::Map::new();
            for value in event["spans"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(std::iter::once(&event["span"]))
                .chain(std::iter::once(&event["fields"]))
            {
                if let Some(object) = value.as_object() {
                    fields.extend(object.clone());
                }
            }
            rows.push(Value::Object(fields));
        }
        rows
    }

    async fn completion(
        &mut self,
        expected: &BTreeSet<String>,
        epoch: &str,
        daemon_pid: &Value,
        warm: bool,
    ) -> Vec<Value> {
        assert!(
            !expected.is_empty(),
            "actual displayed cell must submit compiler work"
        );
        let mut finished = HashMap::new();
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                for row in self.read() {
                    let message = row["message"].as_str().unwrap_or_default();
                    if matches!(message, "compiler request started" | "compiler request finished") {
                        let correlation = row["compile_request"].as_str().unwrap().to_owned();
                        assert!(expected.contains(&correlation), "foreign/concurrent compiler request invalidates exclusive attribution: {row}");
                        assert_eq!(row["daemon_epoch"], epoch, "daemon rotated during production campaign");
                        assert_eq!(&row["daemon_pid"], daemon_pid);
                        if message == "compiler request finished" {
                            assert_eq!(row["transport"], "daemon");
                            assert_eq!(row["exit_code"], 0);
                            assert!(row["worker_pid"].as_u64().is_some_and(|pid| pid > 0));
                            if warm {
                                assert!(row["served"].as_u64().is_some_and(|served| served >= 1), "cold request cannot count as warm: {row}");
                                assert_eq!(row["followed_rotation"], false, "rotated request cannot count as warm");
                            }
                            assert!(finished.insert(correlation, row).is_none(), "duplicate request completion");
                        }
                    }
                }
                if finished.len() == expected.len() { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("actual compiler trace must flush every consumed request");
        finished.into_values().collect()
    }
}

#[tokio::test]
#[ignore = "requires exclusive owned matched daemon and retained TIDEPOOL_PERFORMANCE_COMPILER_TRACE"]
async fn production_engine_store_warm_display_cells_50() {
    let trace_path = std::path::PathBuf::from(
        std::env::var_os("TIDEPOOL_PERFORMANCE_COMPILER_TRACE")
            .expect("explicit actual daemon trace"),
    );
    assert!(trace_path.is_absolute());
    let socket = std::path::PathBuf::from(
        std::env::var_os(tidepool_extract_cmd::DAEMON_SOCKET_ENV)
            .expect("owned production compiler daemon"),
    );
    let endpoint = tidepool_extract_cmd::preflight_compiler_daemon(&socket).unwrap();
    let mut trace = DaemonTrace::open(&trace_path);
    let startups = trace
        .read()
        .into_iter()
        .filter(|row| row["message"] == "compiler daemon ready")
        .collect::<Vec<_>>();
    assert_eq!(
        startups.len(),
        1,
        "one owned daemon boot per retained trace"
    );
    let startup = &startups[0];
    assert_eq!(startup["producer"], endpoint.producer_hex());
    let epoch = startup["daemon_epoch"].as_str().unwrap().to_owned();
    let daemon_pid = startup["daemon_pid"].clone();
    assert!(daemon_pid.as_u64().is_some_and(|pid| pid > 0));
    let clients = ClientRequests::default();
    tracing_subscriber::registry()
        .with(clients.clone())
        .with(
            tracing_subscriber::fmt::layer()
                .with_test_writer()
                .with_filter(tracing_subscriber::EnvFilter::new(
                    "exomonad_actor::workbench_phase=info",
                )),
        )
        .try_init()
        .expect("isolated fixture owns the process tracing subscriber");
    let workloads: Vec<Workload> =
        serde_json::from_str(include_str!("m1_warm_cell_workloads.json")).unwrap();
    assert_eq!(workloads.len(), WARM_UP + MEASURED);
    assert!(
        workloads
            .iter()
            .map(|row| &row.workload)
            .collect::<HashSet<_>>()
            .len()
            >= 10
    );
    assert_eq!(
        workloads
            .iter()
            .skip(WARM_UP)
            .map(|row| &row.source)
            .collect::<HashSet<_>>()
            .len(),
        MEASURED
    );
    let (displayed, mut observations) = mpsc::unbounded_channel();
    let transport = Arc::new(WarmTransport {
        workloads,
        clients: clients.clone(),
        pending: Mutex::new(None),
        next: AtomicUsize::new(0),
        displayed,
    });
    let files = tempfile::tempdir().unwrap();
    let assets = files.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
    let secret_file = files.path().join("browser-secret");
    let secret = "offline-warm-production-cell-secret-is-long-enough";
    std::fs::write(&secret_file, secret).unwrap();
    let auth_file = files.path().join("codex-auth.json");
    std::fs::write(&auth_file, "{}").unwrap();
    let settings = EmbeddedLaunchConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Https,
        asset_root: assets,
        session_secret_file: secret_file,
        codex_auth_file: auth_file,
        context_capacity_tokens: 2_000_000,
        concurrent_jobs: 1,
    };
    let provider: Arc<dyn ResponsesTransport> = transport.clone();
    let fixture = RunningBrowserHost::start(&settings, &provider)
        .await
        .unwrap();
    // Startup belongs to warm-up evidence, never a measured cell's attribution.
    trace.read();
    assert!(
        trace.partial.is_empty(),
        "startup trace must flush before submission"
    );
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
        .unwrap()
        .to_owned();
    let submission = browser_input(
        &browser_target(&fixture.campaign),
        "Measure fifty actual varied Haskell displays.",
    );
    let accepted = client
        .post(format!("{api}/commands"))
        .header("Origin", &origin)
        .header(reqwest::header::COOKIE, &cookie)
        .json(&submission)
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), reqwest::StatusCode::ACCEPTED);
    let mut workers: HashMap<u64, HashSet<usize>> = HashMap::new();
    for sequence in 0..WARM_UP + MEASURED {
        let observation = tokio::time::timeout(Duration::from_secs(300), observations.recv()).await;
        if observation.is_err() {
            let diagnostic = fixture
                .cell_settlement_diagnostic(&format!("warm-resident-cell-{sequence}"))
                .await;
            panic!("actual warm cell {sequence} timed out: {diagnostic}");
        }
        let observed = observation
            .unwrap()
            .expect("actual provider display observation");
        assert_eq!(observed.pending.sequence, sequence);
        let operation = &observed.pending.operation;
        let store = fixture.runtime.store();
        let claims = store.claims(&operation.call).unwrap();
        assert_eq!(
            claims.len(),
            1,
            "one exact original operation per actual cell"
        );
        assert_eq!(&claims[0].operation, operation);
        assert_eq!(claims[0].state, harness::store::ClaimState::Settled);
        assert!(store.replay_output_operation(operation).unwrap().is_some());
        let workload = &transport.workloads[sequence];
        let turns = store.replay_turns(&operation.request).unwrap();
        assert!(
            turns
                .iter()
                .flat_map(|turn| &turn.model_response.items)
                .any(|item| {
                    item.0["type"] == "custom_tool_call"
                        && item.0["call_id"] == operation.call.0
                        && item.0["input"] == workload.source
                }),
            "exact authored source must be durably retained under its original request"
        );
        assert!(observed
            .successor
            .input
            .iter()
            .any(|item| item.0["call_id"] == operation.call.0
                && item.0["type"] == "custom_tool_call_output"));
        let correlations = clients.0.lock()[observed.pending.client_cursor..].to_vec();
        let expected = correlations.iter().cloned().collect::<BTreeSet<_>>();
        assert_eq!(
            expected.len(),
            correlations.len(),
            "one client span per compiler request"
        );
        let measured = sequence >= WARM_UP;
        let requests = trace
            .completion(&expected, &epoch, &daemon_pid, measured)
            .await;
        if measured {
            let index = sequence - WARM_UP;
            for request in requests {
                workers
                    .entry(request["worker_pid"].as_u64().unwrap())
                    .or_default()
                    .insert(index);
            }
            eprintln!(
                "resident-performance {}",
                json!({
                    "schema":1, "composition":"engine-store", "kind":"warm_cell", "index":index,
                    "elapsed_ns":observed.elapsed_ns, "completed":true, "displayed":true,
                    "workload":workload.workload, "source":workload.source, "source_digest":workload.source_digest,
                    "daemon_epoch":epoch, "compiler_requests":correlations, "operation_id":operation,
                    "boundary":"raw-tool-submission-to-engine-returned-display", "compiler_trace":trace_path,
                    "trace_cursor":trace.input.stream_position().unwrap(),
                })
            );
        } else {
            eprintln!(
                "resident-performance-warmup {}",
                json!({"sequence":sequence, "elapsed_ns":observed.elapsed_ns, "operation_id":operation, "compiler_requests":correlations})
            );
        }
        observed.release.send(()).unwrap();
    }
    assert!(!workers.is_empty());
    assert!(
        workers.values().all(|cells| cells.len() >= 2),
        "every participating worker must have measured reuse: {workers:?}"
    );
    fixture
        .stop()
        .await
        .expect("production host and owned resident actors cleanly stop");
    for row in trace.read() {
        assert!(
            !matches!(
                row["message"].as_str(),
                Some("compiler request started" | "compiler request finished")
            ),
            "unattributed compiler work after the final sample invalidates the campaign: {row}"
        );
    }
}
