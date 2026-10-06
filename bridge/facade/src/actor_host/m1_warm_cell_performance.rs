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
const MAX_PENDING_PREPARATION: usize = 128;

#[derive(Clone, serde::Deserialize)]
pub(super) struct Workload {
    pub(super) workload: String,
    pub(super) source: String,
    pub(super) expected: String,
    pub(super) source_digest: String,
}

#[derive(
    Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, serde::Serialize, serde::Deserialize,
)]
pub(super) struct CompilerInvocation {
    daemon_epoch: String,
    admission_id: u64,
    request_ordinal: u64,
    compile_request: String,
}

impl CompilerInvocation {
    fn key(&self) -> (String, u64, u64) {
        (
            self.daemon_epoch.clone(),
            self.admission_id,
            self.request_ordinal,
        )
    }

    pub(super) fn from_trace(row: &Value) -> Self {
        let request: Self =
            serde_json::from_value(row.clone()).expect("complete exact compiler invocation");
        request.validate();
        request
    }

    fn validate(&self) {
        assert!(
            self.admission_id > 0 && self.request_ordinal > 0,
            "one-based daemon invocation counters"
        );
        for (value, length) in [(&self.daemon_epoch, 64), (&self.compile_request, 16)] {
            assert!(
                value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "complete compiler invocation digests"
            );
        }
    }
}

#[derive(Default)]
struct CompilerAttribution {
    issued: HashMap<(String, String), OperationId>,
    executions: HashMap<String, OperationId>,
    requests: HashMap<OperationId, Vec<CompilerInvocation>>,
    owners: HashMap<(String, u64, u64), OperationId>,
}

#[derive(Clone, Default)]
pub(super) struct ClientRequests(Arc<Mutex<CompilerAttribution>>);

impl ClientRequests {
    pub(super) fn issue(&self, operation: &OperationId) {
        let mut state = self.0.lock();
        assert!(
            state.issued.len() < 10_000,
            "bounded operation observations"
        );
        assert!(
            state
                .issued
                .insert(
                    (operation.request.0.clone(), operation.call.0.clone()),
                    operation.clone()
                )
                .is_none(),
            "exact operation issued once"
        );
    }

    pub(super) fn requests(&self, operation: &OperationId) -> Vec<CompilerInvocation> {
        self.0
            .lock()
            .requests
            .get(operation)
            .cloned()
            .unwrap_or_default()
    }
}

#[derive(Default)]
struct TraceFields(HashMap<String, String>);
impl tracing::field::Visit for TraceFields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().into(), format!("{value:?}"));
    }
}

#[derive(Clone)]
struct CellExecution(String);

impl<S> tracing_subscriber::Layer<S> for ClientRequests
where
    S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut fields = TraceFields::default();
        event.record(&mut fields);
        let message = fields.0.get("message").map(String::as_str);
        if message == Some("workbench cell dispatched to its actor") {
            let Some(request) = fields.0.get("turn_id") else {
                return;
            };
            let Some(call) = fields.0.get("context_call_id") else {
                return;
            };
            let mut state = self.0.lock();
            // Startup and non-model cells are outside the authored measurement set.
            let Some(operation) = state.issued.get(&(request.clone(), call.clone())).cloned()
            else {
                return;
            };
            let execution = fields
                .0
                .get("execution")
                .expect("actual dispatch execution identity");
            assert!(!execution.is_empty());
            assert!(
                !state.executions.contains_key(execution),
                "one dispatch per exact execution"
            );
            state.executions.insert(execution.clone(), operation);
            return;
        }
        if message != Some("compiler request identified")
            || event.metadata().target() != "tidepool_extract_cmd::endpoint"
            || fields.0.get("transport").map(String::as_str) != Some("daemon")
        {
            return;
        }
        let execution = context.event_scope(event).and_then(|mut scope| {
            scope.find_map(|ancestor| ancestor.extensions().get::<CellExecution>().cloned())
        });
        let Some(execution) = execution else { return };
        let mut state = self.0.lock();
        let Some(operation) = state.executions.get(&execution.0).cloned() else {
            return;
        };
        let request = CompilerInvocation {
            daemon_epoch: fields
                .0
                .get("daemon_epoch")
                .expect("actual daemon epoch")
                .clone(),
            admission_id: fields
                .0
                .get("admission_id")
                .expect("accepted daemon admission")
                .parse()
                .expect("numeric daemon admission"),
            request_ordinal: fields
                .0
                .get("request_ordinal")
                .expect("actual request ordinal")
                .parse()
                .expect("numeric request ordinal"),
            compile_request: fields
                .0
                .get("compile_request")
                .expect("compiler input digest")
                .clone(),
        };
        request.validate();
        assert!(
            !state.owners.contains_key(&request.key()),
            "exact compiler invocation already belongs to an operation"
        );
        assert!(
            state.requests.get(&operation).map_or(0, Vec::len) < 100,
            "bounded per-operation compiler observations"
        );
        state.owners.insert(request.key(), operation.clone());
        state.requests.entry(operation).or_default().push(request);
    }

    fn on_new_span(
        &self,
        attributes: &tracing::span::Attributes<'_>,
        id: &tracing::Id,
        context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if attributes.metadata().name() != "cell" {
            return;
        }
        let mut fields = TraceFields::default();
        attributes.record(&mut fields);
        if let Some(execution) = fields.0.get("execution").filter(|value| !value.is_empty()) {
            context
                .span(id)
                .unwrap()
                .extensions_mut()
                .insert(CellExecution(execution.clone()));
        }
    }
}

pub(super) fn require_owned_daemon(startup: &Value) {
    assert_eq!(
        startup["daemon_pid"].as_u64().unwrap().to_string(),
        std::env::var("TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PID")
            .expect("owned measurement daemon PID")
    );
    assert_eq!(
        startup["producer"].as_str().unwrap(),
        std::env::var("TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PRODUCER")
            .expect("owned measurement daemon producer")
    );
    assert_eq!(
        startup["daemon_epoch"].as_str().unwrap(),
        std::env::var("TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_EPOCH")
            .expect("actual owned ready epoch")
    );
}

struct PendingCell {
    sequence: usize,
    operation: OperationId,
    started: Instant,
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
                self.clients.issue(&operation);
                *self.pending.lock() = Some(PendingCell {
                    sequence,
                    operation,
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

fn validate_daemon_identity(row: &Value, epoch: &str, daemon_pid: &Value) {
    assert_eq!(
        row["daemon_epoch"], epoch,
        "daemon rotated during production campaign"
    );
    assert_eq!(&row["daemon_pid"], daemon_pid);
}

fn compiler_workload(row: &Value) -> &str {
    let workload = row["compiler_workload"]
        .as_str()
        .unwrap_or_else(|| panic!("compiler request trace lacks typed compiler_workload: {row}"));
    assert!(
        matches!(workload, "foreground" | "preparation"),
        "unknown compiler workload invalidates attribution: {row}"
    );
    workload
}

fn grant_and_slot(row: &Value) -> (u64, u64, u64) {
    let jobs = row["compiler_jobs"]
        .as_u64()
        .unwrap_or_else(|| panic!("compiler trace lacks a numeric compiler_jobs grant: {row}"));
    let capabilities = row["compiler_capabilities"].as_u64().unwrap_or_else(|| {
        panic!("compiler trace lacks a numeric compiler_capabilities grant: {row}")
    });
    let slot = row["worker"]
        .as_u64()
        .unwrap_or_else(|| panic!("compiler trace lacks a numeric worker slot: {row}"));
    assert!(
        jobs > 0 && capabilities > 0,
        "compiler request requires a positive grant: {row}"
    );
    (jobs, capabilities, slot)
}

fn validate_terminal(row: &Value) {
    assert_eq!(row["transport"], "daemon");
    assert_eq!(row["exit_code"], 0);
    assert!(row["worker_pid"].as_u64().is_some_and(|pid| pid > 0));
    grant_and_slot(row);
}

fn validate_request_pair(started: &Value, finished: &Value) {
    assert_eq!(
        CompilerInvocation::from_trace(started),
        CompilerInvocation::from_trace(finished)
    );
    assert_eq!(compiler_workload(started), compiler_workload(finished));
    assert_eq!(started["daemon_epoch"], finished["daemon_epoch"]);
    assert_eq!(started["daemon_pid"], finished["daemon_pid"]);
    assert_eq!(started["worker_pid"], finished["worker_pid"]);
    assert_eq!(grant_and_slot(started), grant_and_slot(finished));
}

fn preparation_opt_in_value(value: Option<&str>) -> bool {
    match value {
        None => false,
        Some(value) => {
            assert_eq!(
                value, "1",
                "preparation attribution opt-in must be exactly `1`"
            );
            true
        }
    }
}

fn preparation_opt_in() -> bool {
    let value = std::env::var("TIDEPOOL_PERFORMANCE_ALLOW_PREPARATION").ok();
    preparation_opt_in_value(value.as_deref())
}

// The cursor reads each trace byte once, retaining an unfinished trailing line.
pub(super) struct DaemonTrace {
    input: BufReader<std::fs::File>,
    partial: String,
    completion_timeout: Duration,
    preparation_pending: HashMap<CompilerInvocation, PendingPreparation>,
    preparation_seen: HashSet<CompilerInvocation>,
}

#[derive(Clone, Debug)]
struct PendingPreparation {
    started: Value,
    start_index: usize,
}

#[derive(Default)]
pub(super) struct CompilerCompletion {
    pub(super) foreground: Vec<Value>,
    pub(super) preparation_background: Vec<PreparationBackground>,
}

#[derive(Clone, Debug)]
pub(super) struct PreparationBackground {
    pub(super) invocation: CompilerInvocation,
    pub(super) start_index: usize,
    pub(super) terminal_index: usize,
    pub(super) started: Value,
    pub(super) finished: Value,
}

impl DaemonTrace {
    pub(super) fn open(path: &std::path::Path) -> Self {
        Self {
            input: BufReader::new(std::fs::File::open(path).expect("retained owned-daemon JSONL")),
            partial: String::new(),
            completion_timeout: Duration::from_secs(30),
            preparation_pending: HashMap::new(),
            preparation_seen: HashSet::new(),
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
            // Preserve the existing logger's time evidence for cross-request
            // service overlap. Missing timestamps remain absent, never inferred.
            if let Some(timestamp) = event.get("timestamp") {
                fields.insert("trace_timestamp".into(), timestamp.clone());
            }
            rows.push(Value::Object(fields));
        }
        rows
    }

    async fn completion(
        &mut self,
        expected: &BTreeSet<CompilerInvocation>,
        epoch: &str,
        daemon_pid: &Value,
        warm: bool,
    ) -> Vec<Value> {
        self.completion_with_preparation(expected, epoch, daemon_pid, warm, None, false)
            .await
            .foreground
    }

    async fn completion_with_preparation(
        &mut self,
        expected: &BTreeSet<CompilerInvocation>,
        epoch: &str,
        daemon_pid: &Value,
        warm: bool,
        sample_index: Option<usize>,
        allow_preparation_background: bool,
    ) -> CompilerCompletion {
        assert!(
            !expected.is_empty(),
            "actual displayed cell must submit compiler work"
        );
        let mut foreground_started = HashMap::<CompilerInvocation, Value>::new();
        let mut foreground = HashMap::new();
        let mut preparation_background = Vec::new();
        tokio::time::timeout(self.completion_timeout, async {
            loop {
                for row in self.read() {
                    let message = row["message"].as_str().unwrap_or_default();
                    assert_ne!(message, "compiler request abandoned by client", "compiler request abandoned during the measured campaign: {row}");
                    assert_ne!(message, "compiler request failed", "compiler request failed during the measured campaign: {row}");
                    if matches!(message, "compiler request started" | "compiler request finished") {
                        let correlation = CompilerInvocation::from_trace(&row);
                        validate_daemon_identity(&row, epoch, daemon_pid);
                        let workload = compiler_workload(&row);
                        let expected_foreground = expected.contains(&correlation);
                        if expected_foreground {
                            assert_eq!(workload, "foreground", "an authored cell request must retain foreground classification: {row}");
                        } else {
                            assert!(allow_preparation_background && workload == "preparation", "foreign/concurrent compiler request invalidates exclusive attribution: {row}");
                        }
                        if message == "compiler request started" {
                            grant_and_slot(&row);
                            if expected_foreground {
                                assert!(foreground_started.insert(correlation, row).is_none(), "duplicate foreground compiler start");
                            } else {
                                let start_index = sample_index.expect("preparation attribution requires a measured sample");
                                assert!(self.preparation_pending.len() < MAX_PENDING_PREPARATION, "bounded preparation background observations");
                                assert!(self.preparation_seen.len() < MAX_PENDING_PREPARATION, "bounded total preparation observations");
                                assert!(self.preparation_seen.insert(correlation.clone()), "physical preparation request started more than once");
                                assert!(self.preparation_pending.insert(correlation, PendingPreparation {
                                    started: row,
                                    start_index,
                                }).is_none(), "duplicate preparation compiler start");
                            }
                        } else {
                            validate_terminal(&row);
                            if expected_foreground {
                                let start = foreground_started.remove(&correlation)
                                    .unwrap_or_else(|| panic!("foreground completion has no exact start: {row}"));
                                validate_request_pair(&start, &row);
                                if warm {
                                    assert!(row["served"].as_u64().is_some_and(|served| served >= 1), "cold foreground request cannot count as warm: {row}");
                                    assert_eq!(row["followed_rotation"], false, "rotated foreground request cannot count as warm");
                                }
                                assert!(foreground.insert(correlation, row).is_none(), "duplicate foreground completion");
                            } else {
                                let pending = self.preparation_pending.remove(&correlation)
                                    .unwrap_or_else(|| panic!("preparation completion has no exact start: {row}"));
                                validate_request_pair(&pending.started, &row);
                                preparation_background.push(PreparationBackground {
                                    invocation: correlation,
                                    start_index: pending.start_index,
                                    terminal_index: sample_index.expect("preparation terminal requires a measured sample"),
                                    started: pending.started,
                                    finished: row,
                                });
                            }
                        }
                    }
                }
                if foreground.len() == expected.len() && foreground_started.is_empty() { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("actual compiler trace must flush every consumed request");
        for (correlation, row) in foreground_started {
            panic!(
                "foreground compiler request has no terminal evidence for {correlation:?}: {row}"
            );
        }
        assert!(
            expected
                .iter()
                .all(|request| foreground.contains_key(request)),
            "foreground compiler request is missing terminal evidence"
        );
        CompilerCompletion {
            foreground: foreground.into_values().collect(),
            preparation_background,
        }
    }

    async fn startup_drain(&mut self, epoch: &str, daemon_pid: &Value) {
        let mut started = HashMap::<CompilerInvocation, Value>::new();
        let quiet = Duration::from_millis(50);
        let deadline = tokio::time::Instant::now() + self.completion_timeout;
        let mut quiet_since = tokio::time::Instant::now();
        loop {
            let rows = self.read();
            if !rows.is_empty() {
                quiet_since = tokio::time::Instant::now();
            }
            for row in rows {
                let message = row["message"].as_str().unwrap_or_default();
                assert_ne!(
                    message, "compiler request abandoned by client",
                    "fixed startup compiler request was abandoned: {row}"
                );
                assert_ne!(
                    message, "compiler request failed",
                    "fixed startup compiler request failed: {row}"
                );
                if !matches!(
                    message,
                    "compiler request started" | "compiler request finished"
                ) {
                    continue;
                }
                let invocation = CompilerInvocation::from_trace(&row);
                validate_daemon_identity(&row, epoch, daemon_pid);
                compiler_workload(&row);
                if message == "compiler request started" {
                    grant_and_slot(&row);
                    assert!(
                        started.insert(invocation, row).is_none(),
                        "duplicate fixed startup compiler request start"
                    );
                } else {
                    validate_terminal(&row);
                    let start = started.remove(&invocation).unwrap_or_else(|| {
                        panic!("fixed startup compiler completion has no exact start: {row}")
                    });
                    validate_request_pair(&start, &row);
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "fixed startup compiler work did not settle: {started:?}"
            );
            if started.is_empty()
                && tokio::time::Instant::now().duration_since(quiet_since) >= quiet
            {
                assert!(
                    self.partial.is_empty(),
                    "fixed startup trace ended with a partial row"
                );
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn final_preparation_drain(
        &mut self,
        epoch: &str,
        daemon_pid: &Value,
        final_sample: usize,
    ) -> Vec<PreparationBackground> {
        let mut settled = Vec::new();
        let deadline = tokio::time::Instant::now() + self.completion_timeout;
        while !self.preparation_pending.is_empty() {
            for row in self.read() {
                let message = row["message"].as_str().unwrap_or_default();
                assert_ne!(
                    message, "compiler request abandoned by client",
                    "preparation background request was abandoned: {row}"
                );
                assert_ne!(
                    message, "compiler request failed",
                    "preparation background request failed: {row}"
                );
                assert_ne!(
                    message, "compiler request started",
                    "new compiler work started after the final sample: {row}"
                );
                if message == "compiler request finished" {
                    let invocation = CompilerInvocation::from_trace(&row);
                    validate_daemon_identity(&row, epoch, daemon_pid);
                    assert_eq!(compiler_workload(&row), "preparation");
                    validate_terminal(&row);
                    let pending =
                        self.preparation_pending
                            .remove(&invocation)
                            .unwrap_or_else(|| {
                                panic!("final preparation terminal has no retained start: {row}")
                            });
                    validate_request_pair(&pending.started, &row);
                    settled.push(PreparationBackground {
                        invocation,
                        start_index: pending.start_index,
                        terminal_index: final_sample,
                        started: pending.started,
                        finished: row,
                    });
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "preparation background did not reach terminal evidence: {:?}",
                self.preparation_pending.keys()
            );
            if !self.preparation_pending.is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        for row in self.read() {
            assert!(
                !is_compiler_request_event(&row),
                "compiler activity appeared after the final preparation drain: {row}"
            );
        }
        assert!(
            self.partial.is_empty(),
            "final compiler trace ended with a partial row"
        );
        settled.sort_by_key(|background| background.invocation.clone());
        settled
    }
}

#[tokio::test]
#[ignore = "requires exclusive owned matched daemon and retained TIDEPOOL_PERFORMANCE_COMPILER_TRACE"]
async fn production_engine_store_warm_display_cells_50() {
    let allow_preparation = preparation_opt_in();
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
    let startup_rows = trace.read();
    assert!(
        startup_rows
            .iter()
            .all(|row| !is_compiler_request_event(row)),
        "compiler activity before fixture warm-up is stale and invalidates the campaign"
    );
    let startups = startup_rows
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
    require_owned_daemon(startup);
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
        public_origin: None,
        asset_root: assets,
        browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret,
        session_secret_file: Some(secret_file),
        provider: crate::exomonad::EmbeddedModelProvider::Codex,
        credential_file: auth_file,
        context_capacity_tokens: 2_000_000,
        concurrent_jobs: 1,
    };
    let provider: Arc<dyn ResponsesTransport> = transport.clone();
    let fixture = HostedTestRuntime::start(&settings, &provider)
        .await
        .unwrap();
    // Fixed startup fixture compilations are validated and drained outside the campaign.
    trace.startup_drain(&epoch, &daemon_pid).await;
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
        &browser_target(&fixture.context),
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
        let claims = store.claims_for_operation(operation).unwrap();
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
        let correlations = clients.requests(operation);
        let expected = correlations.iter().cloned().collect::<BTreeSet<_>>();
        assert_eq!(
            expected.len(),
            correlations.len(),
            "one exact invocation per compiler request"
        );
        let measured = sequence >= WARM_UP;
        let index = sequence.saturating_sub(WARM_UP);
        let completion = trace
            .completion_with_preparation(
                &expected,
                &epoch,
                &daemon_pid,
                measured,
                measured.then_some(index),
                measured && allow_preparation,
            )
            .await;
        if measured {
            for request in completion.foreground {
                workers
                    .entry(request["worker_pid"].as_u64().unwrap())
                    .or_default()
                    .insert(index);
            }
            eprintln!(
                "resident-performance {}",
                json!({
                    "schema":1, "runner_id":"warm_cell", "composition":"engine-store", "kind":"warm_cell", "index":index,
                    "elapsed_ns":observed.elapsed_ns, "completed":true, "displayed":true,
                    "workload":workload.workload, "source":workload.source, "source_digest":workload.source_digest,
                    "daemon_epoch":epoch, "compiler_requests":correlations, "operation_id":operation,
                    "boundary":"raw-tool-submission-to-engine-returned-display", "compiler_trace":trace_path,
                    "trace_cursor":trace.input.stream_position().unwrap(),
                })
            );
            for background in completion.preparation_background {
                eprintln!(
                    "resident-performance-background {}",
                    json!({
                        "schema":1,
                        "runner_id":"warm_cell",
                        "kind":"compiler_preparation_background",
                        "index":background.start_index,
                        "terminal_index":background.terminal_index,
                        "daemon_epoch":epoch,
                        "daemon_pid":daemon_pid,
                        "physical_identity":background.invocation,
                        "started":background.started,
                        "terminal":background.finished,
                        "service_evidence":{
                            "worker_pid":background.finished["worker_pid"],
                            "worker_slot":background.finished["worker"],
                            "compiler_jobs":background.finished["compiler_jobs"],
                            "compiler_capabilities":background.finished["compiler_capabilities"],
                            "served":background.finished["served"],
                            "elapsed_ms":background.finished["elapsed_ms"],
                            "exit_code":background.finished["exit_code"],
                            "followed_rotation":background.finished["followed_rotation"]
                        },
                        "attribution":"separate-from-foreground-cell"
                    })
                );
            }
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
    for background in trace
        .final_preparation_drain(&epoch, &daemon_pid, MEASURED - 1)
        .await
    {
        eprintln!(
            "resident-performance-background {}",
            json!({
                "schema":1,
                "runner_id":"warm_cell",
                "kind":"compiler_preparation_background",
                "index":background.start_index,
                "terminal_index":background.terminal_index,
                "daemon_epoch":epoch,
                "daemon_pid":daemon_pid,
                "physical_identity":background.invocation,
                "started":background.started,
                "terminal":background.finished,
                "service_evidence":{
                    "worker_pid":background.finished["worker_pid"],
                    "worker_slot":background.finished["worker"],
                    "compiler_jobs":background.finished["compiler_jobs"],
                    "compiler_capabilities":background.finished["compiler_capabilities"],
                    "served":background.finished["served"],
                    "elapsed_ms":background.finished["elapsed_ms"],
                    "exit_code":background.finished["exit_code"],
                    "followed_rotation":background.finished["followed_rotation"]
                },
                "attribution":"separate-from-foreground-cell"
            })
        );
    }
}

fn is_compiler_request_event(row: &Value) -> bool {
    matches!(
        row["message"].as_str(),
        Some(
            "compiler request started"
                | "compiler request finished"
                | "compiler request abandoned by client"
                | "compiler request failed"
        )
    )
}

#[path = "m1_compiler_attribution_tests.rs"]
mod attribution_tests;
