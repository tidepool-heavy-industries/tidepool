//! Complete compiled-cell measurements. Each row reports owning counters; compiler
//! interface I/O remains in the independent `TIDEPOOL_TIMING=1` worker trace.

use super::*;
use crate::session::{
    resident_cell_check_template, resident_workbench_templates, CertifiedDeclarationPublication,
    ExecutionPublication, ModuleEnv, OutputSink, PersistentSession, PreparedRuntimeError,
    PublicManifestCommit, PublicationDecision, RecoveryPublicOwner, RecoveryRunAuthority,
    ResidentError, ResidentSession, SessionLib, SessionRunContext, SourceImports,
};
use parking_lot::Mutex as CaptureMutex;
use sha2::Digest;
use std::collections::HashSet;
use std::time::{Duration, Instant};
use tidepool_codegen::{prepared_program::ImageRegistry, scope::ScopeId};
use tidepool_repr::SessionId;
use tidepool_testing::effect_surface::TestEffectSurface;
use tidepool_toolchain::checked_cell::CheckedItemKind;
use tracing_subscriber::prelude::*;

const MAX_CAPTURED_CELLS: usize = 512;
const MAX_REQUESTS_PER_CELL: usize = 64;
const MAX_CAPTURED_SOURCE_BYTES: usize = 64 * 1024;
const MAX_HOST_TIMINGS_PER_CELL: usize = 512;
const MAX_HOST_TIMING_LABEL_BYTES: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
struct CompilerRequest {
    daemon_epoch: String,
    admission_id: u64,
    request_ordinal: u64,
    compile_request: String,
}

impl CompilerRequest {
    fn key(&self) -> (String, u64, u64) {
        (
            self.daemon_epoch.clone(),
            self.admission_id,
            self.request_ordinal,
        )
    }
}

#[derive(Default)]
struct RequestFields(std::collections::HashMap<String, String>);

impl tracing::field::Visit for RequestFields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.insert(field.name().into(), value.into());
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().into(), format!("{value:?}"));
    }
}

#[derive(Default)]
struct RequestState {
    active: Option<CellCaptureState>,
    next_cell: u64,
    owners: HashSet<(String, u64, u64)>,
}

struct CellCaptureState {
    index: u64,
    label: String,
    compiler_requests: Vec<CompilerRequest>,
    host_timings: Vec<HostTimingObservation>,
    dropped_host_timings: u64,
}

impl CellCaptureState {
    fn new(index: u64, label: &str) -> Self {
        Self {
            index,
            label: label.into(),
            compiler_requests: Vec::new(),
            host_timings: Vec::new(),
            dropped_host_timings: 0,
        }
    }
}

#[derive(Debug, PartialEq, serde::Serialize)]
struct HostTimingObservation {
    ordinal: u64,
    // This orders host events against physical submissions without assigning
    // pre-submission work to a previous or speculative future request.
    identified_request_count: usize,
    measurement: HostTimingMeasurement,
}

#[derive(Debug, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum HostTimingMeasurement {
    ToolchainStage {
        stage: String,
        elapsed_ms: u64,
        payload_bytes: u64,
        owners: Option<u64>,
        node: String,
        round: String,
    },
    CompilerPhase {
        phase: String,
        elapsed_ms: u64,
        accepted: Option<bool>,
        success: Option<bool>,
    },
}

#[derive(Default)]
struct HostTimingFields {
    stage: Option<String>,
    phase: Option<String>,
    node: Option<String>,
    round: Option<String>,
    ms: Option<u64>,
    elapsed_ms: Option<u64>,
    bytes: Option<u64>,
    owners: Option<u64>,
    owners_known: Option<bool>,
    accepted: Option<bool>,
    success: Option<bool>,
    phase_present: bool,
    unreadable_field: bool,
}

impl HostTimingFields {
    fn reject_type(&mut self, field: &tracing::field::Field) {
        self.phase_present |= field.name() == "phase";
        self.unreadable_field |= matches!(
            field.name(),
            "stage"
                | "phase"
                | "node"
                | "round"
                | "ms"
                | "elapsed_ms"
                | "bytes"
                | "owners"
                | "owners_known"
                | "accepted"
                | "success"
        );
    }
}

impl tracing::field::Visit for HostTimingFields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.phase_present |= field.name() == "phase";
        let slot = match field.name() {
            "stage" => &mut self.stage,
            "phase" => &mut self.phase,
            "node" => &mut self.node,
            "round" => &mut self.round,
            _ => return self.reject_type(field),
        };
        if value.len() > MAX_HOST_TIMING_LABEL_BYTES {
            self.unreadable_field = true;
        } else {
            *slot = Some(value.into());
        }
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        match field.name() {
            "ms" => self.ms = Some(value),
            "elapsed_ms" => self.elapsed_ms = Some(value),
            "bytes" => self.bytes = Some(value),
            "owners" => self.owners = Some(value),
            _ => self.reject_type(field),
        }
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        match field.name() {
            "owners_known" => self.owners_known = Some(value),
            "accepted" => self.accepted = Some(value),
            "success" => self.success = Some(value),
            _ => self.reject_type(field),
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, _: &dyn std::fmt::Debug) {
        self.reject_type(field);
    }
}

#[derive(Clone, Default)]
struct CellRequestObserver(Arc<CaptureMutex<RequestState>>);

struct CapturedCell {
    index: u64,
    label: String,
    source_sha256: String,
    source_path: PathBuf,
    compiler_requests: Vec<CompilerRequest>,
    host_timings: Vec<HostTimingObservation>,
    dropped_host_timings: u64,
}

struct ActiveCell<'a> {
    observer: &'a CellRequestObserver,
    index: u64,
    label: String,
    source_sha256: String,
    source_path: PathBuf,
    completed: bool,
}

impl CellRequestObserver {
    fn capture_host_timing(&self, event: &tracing::Event<'_>) -> bool {
        let mut fields = HostTimingFields::default();
        event.record(&mut fields);
        let toolchain_stage = event.metadata().target() == "exomonad_harness::timing";
        if !toolchain_stage && !fields.phase_present {
            return false;
        }
        let mut state = self.0.lock();
        let Some(active) = state.active.as_mut() else {
            return true;
        };
        let ordinal =
            (active.host_timings.len() as u64).saturating_add(active.dropped_host_timings);
        let measurement = (|| {
            if fields.unreadable_field {
                return None;
            }
            Some(if toolchain_stage {
                HostTimingMeasurement::ToolchainStage {
                    stage: fields.stage?,
                    elapsed_ms: fields.ms?,
                    payload_bytes: fields.bytes?,
                    owners: match fields.owners_known? {
                        true => Some(fields.owners?),
                        false => None,
                    },
                    node: fields.node?,
                    round: fields.round?,
                }
            } else {
                HostTimingMeasurement::CompilerPhase {
                    phase: fields.phase?,
                    elapsed_ms: fields.elapsed_ms?,
                    accepted: fields.accepted,
                    success: fields.success,
                }
            })
        })();
        if active.host_timings.len() == MAX_HOST_TIMINGS_PER_CELL || measurement.is_none() {
            active.dropped_host_timings = active.dropped_host_timings.saturating_add(1);
            return true;
        }
        active.host_timings.push(HostTimingObservation {
            ordinal,
            identified_request_count: active.compiler_requests.len(),
            measurement: measurement.expect("validated typed host timing fields"),
        });
        true
    }

    fn begin(&self, label: &str, source: &str) -> ActiveCell<'_> {
        assert!(
            source.len() <= MAX_CAPTURED_SOURCE_BYTES,
            "bounded authored source"
        );
        let artifact_root = PathBuf::from(
            std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT")
                .expect("isolated test runner must retain TIDEPOOL_TEST_ARTIFACT_ROOT"),
        );
        assert!(artifact_root.is_absolute());
        let mut state = self.0.lock();
        assert!(state.active.is_none(), "fixture submits cells sequentially");
        assert!(
            state.next_cell < MAX_CAPTURED_CELLS as u64,
            "bounded cell capture"
        );
        let index = state.next_cell;
        state.next_cell += 1;
        let run_root = tempfile::Builder::new()
            .prefix("resident-cell-correlation-")
            .tempdir_in(&artifact_root)
            .unwrap()
            .keep();
        let source_path = run_root.join(format!("cell-{index:04}.hs"));
        std::fs::write(&source_path, source.as_bytes()).unwrap();
        let source_sha256 = format!("{:x}", sha2::Sha256::digest(source.as_bytes()));
        state.active = Some(CellCaptureState::new(index, label));
        ActiveCell {
            observer: self,
            index,
            label: label.into(),
            source_sha256,
            source_path,
            completed: false,
        }
    }
}

fn with_cell_request_capture<T>(
    observer: &CellRequestObserver,
    phase: &str,
    label: &str,
    source: &str,
    action: impl FnOnce() -> Result<T, ResidentError>,
) -> (Result<T, ResidentError>, CapturedCell) {
    let active = observer.begin(label, source);
    let span = tracing::info_span!("resident_compiler_cell", label = %label);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tracing::subscriber::with_default(
            tracing_subscriber::registry().with(observer.clone()),
            || span.in_scope(action),
        )
    }));
    let captured = active.finish();
    eprintln!(
        "resident-cell-correlation {}",
        serde_json::json!({
            "schema": 1, "index": captured.index, "label": &captured.label,
            "source_path": captured.source_path.display().to_string(),
            "source_sha256": &captured.source_sha256,
            "source_blake3": blake3::hash(source.as_bytes()).to_hex().to_string(),
            "compiler_requests": &captured.compiler_requests,
            "host_timings": &captured.host_timings,
            "host_timings_dropped": captured.dropped_host_timings,
            "host_timings_complete": captured.dropped_host_timings == 0,
            "completed": matches!(&result, Ok(Ok(_))), "phase": phase,
        })
    );
    match result {
        Ok(result) => (result, captured),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

impl ActiveCell<'_> {
    fn finish(mut self) -> CapturedCell {
        let mut state = self.observer.0.lock();
        let captured = state.active.take().expect("active measured cell");
        assert_eq!(captured.index, self.index);
        assert_eq!(captured.label, self.label);
        self.completed = true;
        CapturedCell {
            index: captured.index,
            label: captured.label,
            source_sha256: self.source_sha256.clone(),
            source_path: self.source_path.clone(),
            compiler_requests: captured.compiler_requests,
            host_timings: captured.host_timings,
            dropped_host_timings: captured.dropped_host_timings,
        }
    }
}

impl Drop for ActiveCell<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.observer.0.lock().active = None;
        }
    }
}

impl<S> tracing_subscriber::Layer<S> for CellRequestObserver
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if matches!(
            event.metadata().target(),
            "exomonad_harness::timing" | "tidepool_extract_cmd::daemon"
        ) {
            self.capture_host_timing(event);
            return;
        }
        if event.metadata().target() != "tidepool_extract_cmd::endpoint" {
            return;
        }
        if self.capture_host_timing(event) {
            return;
        }
        let mut fields = RequestFields::default();
        event.record(&mut fields);
        let get = |name: &str| fields.0.get(name).map(String::as_str);
        if get("message") != Some("compiler request identified")
            || get("transport") != Some("daemon")
        {
            return;
        }
        if self.0.lock().active.is_none() {
            return;
        }
        let request = CompilerRequest {
            daemon_epoch: fields
                .0
                .get("daemon_epoch")
                .expect("daemon epoch field")
                .clone(),
            admission_id: fields
                .0
                .get("admission_id")
                .expect("admission field")
                .parse()
                .unwrap(),
            request_ordinal: fields
                .0
                .get("request_ordinal")
                .expect("request ordinal field")
                .parse()
                .unwrap(),
            compile_request: fields
                .0
                .get("compile_request")
                .expect("compile request field")
                .clone(),
        };
        assert!(request.admission_id > 0 && request.request_ordinal > 0);
        assert!(
            request.daemon_epoch.len() == 64
                && request.daemon_epoch.bytes().all(|b| b.is_ascii_hexdigit())
        );
        assert!(
            request.compile_request.len() == 16
                && request
                    .compile_request
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit())
        );
        let mut state = self.0.lock();
        if state.active.is_none() {
            return;
        }
        assert!(
            state.owners.insert(request.key()),
            "physical daemon request has one cell owner"
        );
        let Some(active) = state.active.as_mut() else {
            return;
        };
        assert!(
            active.compiler_requests.len() < MAX_REQUESTS_PER_CELL,
            "bounded requests per measured cell"
        );
        active.compiler_requests.push(request);
    }
}

#[cfg(test)]
mod cell_request_observer_tests {
    use super::*;

    fn active<'a>(observer: &'a CellRequestObserver, label: &str) -> ActiveCell<'a> {
        let mut state = observer.0.lock();
        assert!(state.active.is_none());
        let index = state.next_cell;
        state.next_cell += 1;
        state.active = Some(CellCaptureState::new(index, label));
        drop(state);
        ActiveCell {
            observer,
            index,
            label: label.into(),
            source_sha256: "a".repeat(64),
            source_path: PathBuf::from(format!("cell-{index}.hs")),
            completed: false,
        }
    }

    fn identified(admission_id: u64, request_ordinal: u64) {
        tracing::info!(target: "tidepool_extract_cmd::endpoint",
            daemon_epoch = %"a".repeat(64), admission_id, request_ordinal,
            compile_request = "0123456789abcdef", transport = "daemon",
            "compiler request identified");
    }

    #[test]
    fn cell_capture_retains_existing_typed_host_timings_in_logical_order() {
        let observer = CellRequestObserver::default();
        tracing::subscriber::with_default(
            tracing_subscriber::registry().with(observer.clone()),
            || {
                let stage = || {
                    tidepool_toolchain::timing::record_stage(
                        tidepool_toolchain::timing::NO_NODE,
                        tidepool_toolchain::timing::NO_ROUND,
                        "future.context_stage",
                        Duration::from_millis(7),
                        123,
                    )
                };
                stage();
                let cell = active(&observer, "host-timings");
                stage();
                tracing::info!(target: "tidepool_extract_cmd::daemon",
                    phase = "compiler_preflight", elapsed_ms = 2_u64,
                    "unparsed event wording");
                tracing::info!(target: "tidepool_extract_cmd::endpoint",
                    phase = "compiler_transaction_admission", elapsed_ms = 3_u64,
                    "unparsed admission wording");
                identified(1, 1);
                tracing::info!(target: "tidepool_extract_cmd::daemon",
                    phase = "compiler_response", elapsed_ms = 11_u64,
                    success = true, "unparsed response wording");
                let captured = cell.finish();
                assert_eq!(captured.host_timings.len(), 4);
                assert_eq!(captured.dropped_host_timings, 0);
                assert_eq!(captured.host_timings[0].ordinal, 0);
                assert_eq!(captured.host_timings[0].identified_request_count, 0);
                assert!(matches!(
                    &captured.host_timings[0].measurement,
                    HostTimingMeasurement::ToolchainStage {
                        stage, elapsed_ms: 7, payload_bytes: 123, owners: None, ..
                    } if stage == "future.context_stage"
                ));
                assert_eq!(captured.host_timings[2].ordinal, 2);
                assert_eq!(captured.host_timings[2].identified_request_count, 0);
                assert!(matches!(
                    &captured.host_timings[2].measurement,
                    HostTimingMeasurement::CompilerPhase {
                        phase, elapsed_ms: 3, success: None, accepted: None,
                    } if phase == "compiler_transaction_admission"
                ));
                assert_eq!(captured.compiler_requests.len(), 1);
                assert_eq!(captured.host_timings[3].ordinal, 3);
                assert_eq!(captured.host_timings[3].identified_request_count, 1);
                assert!(matches!(
                    &captured.host_timings[3].measurement,
                    HostTimingMeasurement::CompilerPhase {
                        phase, elapsed_ms: 11, success: Some(true), accepted: None,
                    } if phase == "compiler_response"
                ));
                stage();
                let next = active(&observer, "next").finish();
                assert!(next.host_timings.is_empty());
            },
        );
    }

    #[test]
    fn cell_capture_marks_wrong_typed_present_fields_incomplete() {
        let observer = CellRequestObserver::default();
        tracing::subscriber::with_default(
            tracing_subscriber::registry().with(observer.clone()),
            || {
                let cell = active(&observer, "wrong-types");
                tracing::info!(target: "tidepool_extract_cmd::daemon",
                    phase = 1_i64, elapsed_ms = 2_u64, "signed phase");
                tracing::info!(target: "tidepool_extract_cmd::endpoint",
                    phase = %"compiler_transaction_admission", elapsed_ms = 2_u64,
                    "display phase");
                tracing::info!(target: "tidepool_extract_cmd::daemon",
                    phase = "future.phase", elapsed_ms = 2_i64, "signed elapsed");
                tracing::info!(target: "tidepool_extract_cmd::daemon",
                    phase = "future.phase", elapsed_ms = 2_f64, "float elapsed");
                tracing::info!(target: "tidepool_extract_cmd::daemon",
                    phase = "future.phase", elapsed_ms = "2", "string elapsed");
                tracing::info!(target: "tidepool_extract_cmd::daemon",
                    phase = "future.phase", elapsed_ms = 2_u64, accepted = "true",
                    "string optional bool");
                tracing::info!(target: "tidepool_extract_cmd::daemon",
                    phase = "future.phase", elapsed_ms = 2_u64, success = 1_i64,
                    "signed optional bool");
                tracing::info!(target: "tidepool_extract_cmd::daemon",
                    phase = "future.phase", elapsed_ms = 2_u64,
                    "valid absent optional bools");
                let captured = cell.finish();
                assert_eq!(captured.dropped_host_timings, 7);
                assert_eq!(captured.host_timings.len(), 1);
                assert_eq!(captured.host_timings[0].ordinal, 7);
                assert!(matches!(
                    &captured.host_timings[0].measurement,
                    HostTimingMeasurement::CompilerPhase {
                        elapsed_ms: 2,
                        accepted: None,
                        success: None,
                        ..
                    }
                ));
            },
        );
    }

    #[test]
    fn cell_capture_reports_bounded_or_unreadable_host_timings() {
        let observer = CellRequestObserver::default();
        tracing::subscriber::with_default(
            tracing_subscriber::registry().with(observer.clone()),
            || {
                let cell = active(&observer, "bounded");
                tracing::info!(target: "tidepool_extract_cmd::daemon",
                    phase = "future.phase", "missing typed elapsed field");
                for _ in 0..MAX_HOST_TIMINGS_PER_CELL + 3 {
                    tidepool_toolchain::timing::record_stage(
                        tidepool_toolchain::timing::NO_NODE,
                        tidepool_toolchain::timing::NO_ROUND,
                        "context.stage",
                        Duration::ZERO,
                        0,
                    );
                }
                let captured = cell.finish();
                assert_eq!(captured.host_timings.len(), MAX_HOST_TIMINGS_PER_CELL);
                assert_eq!(captured.dropped_host_timings, 4);
                assert_eq!(captured.host_timings[0].ordinal, 1);
                assert_eq!(
                    captured.host_timings.last().unwrap().ordinal,
                    MAX_HOST_TIMINGS_PER_CELL as u64
                );
            },
        );
    }

    #[test]
    fn cell_requests_belong_to_the_sequential_active_cell() {
        let observer = CellRequestObserver::default();
        tracing::subscriber::with_default(
            tracing_subscriber::registry().with(observer.clone()),
            || {
                let first = active(&observer, "first");
                identified(1, 1);
                let first = first.finish();
                assert_eq!(first.compiler_requests.len(), 1);
                assert_eq!(first.compiler_requests[0].admission_id, 1);

                let second = active(&observer, "second");
                identified(2, 1);
                let second = second.finish();
                assert_eq!(second.compiler_requests.len(), 1);
                assert_eq!(second.compiler_requests[0].admission_id, 2);
            },
        );
    }

    #[test]
    fn cell_capture_preserves_empty_and_rejects_malformed_or_duplicate_requests() {
        let observer = CellRequestObserver::default();
        tracing::subscriber::with_default(
            tracing_subscriber::registry().with(observer.clone()),
            || {
                let empty = active(&observer, "empty");
                assert!(empty.finish().compiler_requests.is_empty());

                let malformed = active(&observer, "malformed");
                let malformed_event = || {
                    tracing::info!(target: "tidepool_extract_cmd::endpoint",
                        daemon_epoch = %"a".repeat(64), admission_id = 2_u64,
                        transport = "daemon", "compiler request identified");
                };
                assert!(
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(malformed_event))
                        .is_err()
                );
                drop(malformed);

                let duplicate = active(&observer, "duplicate");
                identified(3, 1);
                assert!(
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| identified(3, 1)))
                        .is_err()
                );
                let captured = duplicate.finish();
                assert_eq!(captured.compiler_requests.len(), 1);
            },
        );
    }
}

#[derive(Clone)]
pub(in crate::session) struct QuietOutput;

impl OutputSink for QuietOutput {
    fn drain(&self) -> Vec<String> {
        Vec::new()
    }

    fn snapshot(&self) -> Vec<String> {
        Vec::new()
    }
}

pub(in crate::session) type ScaleSession = ResidentSession<frunk::HNil, QuietOutput>;

/// Choose the existing publication owner; durable measurements never use the
/// ephemeral publication shortcut.
pub(in crate::session) enum ScalePublication {
    Ephemeral,
    Durable {
        owner: RecoveryPublicOwner,
        manifest: PathBuf,
    },
}

/// Hold the reference workspace's real exclusive file lock for the session.
struct ScaleRunOwner {
    root: PathBuf,
    _lock: std::fs::File,
}
impl RecoveryRunAuthority for ScaleRunOwner {
    fn owns_run(&self, root: &Path) -> std::io::Result<bool> {
        Ok(root.canonicalize()? == self.root)
    }
}
fn scale_run_owner(root: &Path) -> Arc<ScaleRunOwner> {
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("performance-run-owner.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    Arc::new(ScaleRunOwner {
        root: root.canonicalize().unwrap(),
        _lock: lock,
    })
}

fn scale_workspace(durable: bool) -> (PathBuf, Option<tempfile::TempDir>) {
    let root = if durable {
        let parent = PathBuf::from(
            std::env::var_os("TIDEPOOL_PERFORMANCE_WORKSPACE_ROOT")
                .expect("durable evidence requires an explicit retained workspace parent"),
        );
        assert!(parent.is_absolute());
        tempfile::Builder::new()
            .prefix("durable-workspace-")
            .tempdir_in(parent)
            .unwrap()
    } else {
        tempfile::tempdir().unwrap()
    };
    let path = root.path().to_path_buf();
    if durable {
        eprintln!("durable-workspace retained={}", root.keep().display());
        (path, None)
    } else {
        (path, Some(root))
    }
}

fn counters(resident: &ScaleSession, images: &ImageRegistry) -> serde_json::Value {
    let residency = resident.residency().unwrap_or_default();
    let (functions, code_bytes) = resident.codegen_totals().unwrap_or_default();
    serde_json::json!({
        "compiler_submissions": tidepool_extract_cmd::extract_spawn_count(),
        "image_elections": images.misses(), "image_hits": images.hits(),
        "codegen_functions": functions, "codegen_bytes": code_bytes,
        "programs": residency.programs, "block_words": residency.block_words,
        "persistent_roots": residency.persistent_roots, "handles": residency.handles,
        "code_exports": residency.code_exports, "parked": residency.parked,
        "static_regions": residency.static_regions,
        "descriptor_rows": residency.descriptor_rows,
        "callable_rows": residency.callable_rows, "enter_rows": residency.enter_rows,
    })
}

fn measured_duration<T>(
    resident: &mut ScaleSession,
    images: &ImageRegistry,
    scenario: (usize, usize),
    phase: &str,
    item: Option<usize>,
    action: impl FnOnce(&mut ScaleSession) -> T,
) -> (T, u128) {
    let before = counters(resident, images);
    let started = Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| action(resident)));
    let elapsed_ns = started.elapsed().as_nanos();
    eprintln!(
        "protected-scale {}",
        serde_json::json!({
            "schema": 1, "prefix": scenario.0, "baseline": scenario.1,
            "phase": phase, "item": item, "elapsed_ns": elapsed_ns,
            "completed": result.is_ok(),
            "before": before, "after": counters(resident, images),
        })
    );
    match result {
        Ok(value) => (value, elapsed_ns),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

fn measured<T>(
    resident: &mut ScaleSession,
    images: &ImageRegistry,
    scenario: (usize, usize),
    phase: &str,
    item: Option<usize>,
    action: impl FnOnce(&mut ScaleSession) -> T,
) -> T {
    measured_duration(resident, images, scenario, phase, item, action).0
}

pub(in crate::session) fn execute_cell(
    resident: &mut ScaleSession,
    public: ScopeId,
    effects: &TestEffectSurface,
    images: &ImageRegistry,
    scenario: (usize, usize),
    label: &str,
    source: &str,
    declarations: usize,
    publication_target: &ScalePublication,
) -> Duration {
    execute_cell_with_authority_checks(
        resident,
        public,
        effects,
        images,
        scenario,
        label,
        source,
        declarations,
        publication_target,
        AuthorityChecks::Configured,
    )
}

#[derive(Clone, Copy)]
enum AuthorityChecks {
    Configured,
    RefusalBranches,
}

fn execute_cell_with_authority_checks(
    resident: &mut ScaleSession,
    public: ScopeId,
    effects: &TestEffectSurface,
    images: &ImageRegistry,
    scenario: (usize, usize),
    label: &str,
    source: &str,
    declarations: usize,
    publication_target: &ScalePublication,
    authority_checks: AuthorityChecks,
) -> Duration {
    try_execute_cell_with_authority_checks(
        resident,
        public,
        effects,
        images,
        scenario,
        label,
        source,
        declarations,
        publication_target,
        authority_checks,
    )
    .unwrap()
}

fn try_execute_cell_with_authority_checks(
    resident: &mut ScaleSession,
    public: ScopeId,
    effects: &TestEffectSurface,
    images: &ImageRegistry,
    scenario: (usize, usize),
    label: &str,
    source: &str,
    declarations: usize,
    publication_target: &ScalePublication,
    authority_checks: AuthorityChecks,
) -> Result<Duration, ResidentError> {
    try_execute_cell_with_template_imports(
        resident,
        public,
        effects,
        images,
        scenario,
        label,
        source,
        declarations,
        publication_target,
        authority_checks,
        &SourceImports::new(),
    )
    .map(|(elapsed, _)| elapsed)
}

fn try_execute_cell_with_template_imports(
    resident: &mut ScaleSession,
    public: ScopeId,
    effects: &TestEffectSurface,
    images: &ImageRegistry,
    scenario: (usize, usize),
    label: &str,
    source: &str,
    declarations: usize,
    publication_target: &ScalePublication,
    authority_checks: AuthorityChecks,
    template_imports: &SourceImports,
) -> Result<(Duration, Vec<Arc<PreparedProgram>>), ResidentError> {
    try_execute_cell_with_template_imports_expectation(
        resident,
        public,
        effects,
        images,
        scenario,
        label,
        source,
        declarations,
        publication_target,
        authority_checks,
        template_imports,
        None,
    )
}

fn try_execute_cell_with_template_imports_expectation(
    resident: &mut ScaleSession,
    public: ScopeId,
    effects: &TestEffectSurface,
    images: &ImageRegistry,
    scenario: (usize, usize),
    label: &str,
    source: &str,
    declarations: usize,
    publication_target: &ScalePublication,
    authority_checks: AuthorityChecks,
    template_imports: &SourceImports,
    expected_observation: Option<i64>,
) -> Result<(Duration, Vec<Arc<PreparedProgram>>), ResidentError> {
    let cell_started = Instant::now();
    let mut expected_public_winners: std::collections::BTreeMap<_, _> = resident
        .public_visibility_snapshot_in(public)
        .unwrap()
        .bindings
        .into_iter()
        .collect();
    let execution = Arc::new(resident.begin_private_execution(public).unwrap());
    let view = execution.view();
    let imports = view.turn_imports(template_imports);
    let template = resident_cell_check_template(effects.preamble(), effects.row(), &imports);
    let templates = resident_workbench_templates(effects.preamble(), effects.row(), &imports);
    let specification = CheckedCellSpecification {
        admission_digest: [0; 32],
        cell_source: source.into(),
        template_source: template.clone(),
        turn_templates: templates
            .iter()
            .map(|template| (template.kind.wire_name().into(), template.source.clone()))
            .collect(),
        injected_modules: view.injected_module_names(),
        reserved_declaration_modules: Vec::new(),
    };
    let admitted_include = view.include_paths(effects.include_paths());
    let plan = measured(
        resident,
        images,
        scenario,
        &format!("{label}.parse"),
        None,
        |_| {
            tidepool_toolchain::artifacts::parse_cell_plan(
                Arc::new(specification.clone()),
                &admitted_include,
            )
            .unwrap()
        },
    );
    assert_eq!(
        plan.items()
            .iter()
            .filter(|item| matches!(
                item.kind(),
                tidepool_toolchain::cell_plan::ParsedCellPlanKind::Prologue
                    | tidepool_toolchain::cell_plan::ParsedCellPlanKind::Declaration
            ))
            .count(),
        declarations
    );
    let admission = resident
        .admit_planned_cell_for_execution(
            execution.clone(),
            plan,
            Arc::new(specification.clone()),
            specification.specification_digest(),
            [1; 32],
            admitted_include,
            None,
        )
        .unwrap();
    let view = admission.view();
    let include = view.include_paths(effects.include_paths());
    let include = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let injected = view.injected_module_names();
    let compile_cell = || {
        compile_cell_program_admitted(
            CellCheckRequest {
                exact_context: view.exact_compile_context(),
                session_id: Some(view.session()),
                cell_text: source,
                template: &template,
                include: &include,
                session_root: view.session_root(),
                inject_modules: &injected,
                compile_generation: admission.initial_value_generation().0,
                compile_view_evidence: "",
            },
            admission.clone(),
            &templates,
        )
    };
    if matches!(authority_checks, AuthorityChecks::RefusalBranches) {
        struct RestoreDeployment(std::ffi::OsString);
        impl Drop for RestoreDeployment {
            fn drop(&mut self) {
                std::env::set_var("TIDEPOOL_COMPILER_DEPLOYMENT", &self.0);
            }
        }
        let configured = std::env::var_os("TIDEPOOL_COMPILER_DEPLOYMENT")
            .expect("vertical test requires configured deployment");
        let guard = RestoreDeployment(configured.clone());
        let before = tidepool_extract_cmd::extract_spawn_count();
        std::env::remove_var("TIDEPOOL_COMPILER_DEPLOYMENT");
        assert!(
            compile_cell().is_err(),
            "unconfigured planned compile must refuse admission"
        );
        assert_eq!(tidepool_extract_cmd::extract_spawn_count(), before);
        let mut mismatch: serde_json::Value =
            serde_json::from_slice(&std::fs::read(configured).unwrap()).unwrap();
        let producer = mismatch["producer_identity"].as_array_mut().unwrap();
        producer[0] = serde_json::json!(producer[0].as_u64().unwrap() ^ 1);
        let wrong = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(wrong.path(), serde_json::to_vec(&mismatch).unwrap()).unwrap();
        std::env::set_var("TIDEPOOL_COMPILER_DEPLOYMENT", wrong.path());
        assert!(
            compile_cell().is_err(),
            "wrong producer planned compile must refuse admission"
        );
        assert_eq!(tidepool_extract_cmd::extract_spawn_count(), before);
        drop(guard);
    }
    let (checked, program) = measured(
        resident,
        images,
        scenario,
        &format!("{label}.compile_cell"),
        None,
        |_| compile_cell().unwrap(),
    );
    assert!(!checked.items.is_empty());
    eprintln!(
        "protected-scale {}",
        serde_json::json!({
            "schema": 1, "prefix": scenario.0, "baseline": scenario.1,
            "phase": format!("{label}.checked_inventory"),
            "binder_counts": (0..checked.items.len())
                .map(|index| checked.checked_item(index).unwrap().binders().len())
                .collect::<Vec<_>>(),
        })
    );
    // Keep the actual compiler products for owner tests that inspect structural
    // link contracts. These are observations of admitted output, not authority.
    let native_targets = program
        .items()
        .iter()
        .filter_map(|item| item.native().map(|native| native.target_owned()))
        .collect();
    let prefix = resident
        .begin_cell_program(admission, program)
        .unwrap()
        .expect("nonempty compiled cell has an ordered prefix");
    let submissions_before_effects = tidepool_extract_cmd::extract_spawn_count();
    let mut expected_observation_seen = false;
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: execution.private_scope(),
            ..Default::default()
        })
        .unwrap();
    for index in 0..checked.items.len() {
        let item = checked.checked_item(index).unwrap();
        let reservation = resident
            .admit_checked_item(prefix.clone(), item.clone())
            .unwrap();
        if item.kind() == CheckedItemKind::Declaration {
            measured(
                resident,
                images,
                scenario,
                &format!("{label}.adopt"),
                Some(index),
                |resident| {
                    resident.adopt_checked_declaration(reservation).unwrap();
                },
            );
        } else {
            let TurnResult::Bind {
                bound, compiled, ..
            } = measured(
                resident,
                images,
                scenario,
                &format!("{label}.native_materialize"),
                Some(index),
                |_| consume_cell_program_item(reservation.clone()).unwrap(),
            )
            else {
                panic!("checked native item did not return its binding recipe")
            };
            if item.kind() == CheckedItemKind::Bind {
                assert_eq!(
                    bound
                        .iter()
                        .map(|binder| binder.name.as_str())
                        .collect::<Vec<_>>(),
                    item.binders()
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>(),
                    "native recipe must preserve the complete checked binder inventory"
                );
                measured(
                    resident,
                    images,
                    scenario,
                    &format!("{label}.native_bind"),
                    Some(index),
                    |resident| {
                        if bound.len() == 1 {
                            resident
                                .run_bind_with_sites(
                                    &bound[0].name,
                                    compiled.code(),
                                    &bound[0],
                                    reservation.generation(),
                                )
                                .unwrap();
                        } else {
                            assert!(!bound.is_empty());
                            resident
                                .run_projected_bind_with_sites(
                                    label,
                                    compiled.code(),
                                    &bound,
                                    reservation.generation(),
                                )
                                .unwrap();
                        }
                    },
                );
            } else {
                let observed = measured(
                    resident,
                    images,
                    scenario,
                    &format!("{label}.native_observe"),
                    Some(index),
                    |resident| {
                        resident.run_observation_with_sites(
                            compiled.code(),
                            &bound[0],
                            reservation.generation(),
                            false,
                        )
                    },
                );
                assert_eq!(
                    tidepool_extract_cmd::extract_spawn_count(),
                    submissions_before_effects
                );
                let outcome = observed?;
                if let Some(expected) = expected_observation {
                    assert!(
                        matches!(outcome, crate::session::ResidentOutcome::Completed { .. }),
                        "guarded integer {expected} capture did not complete: {outcome:?}"
                    );
                    assert_eq!(bound.len(), 1, "one exact native observation binder");
                    let installed = resident
                        .current_binding_in(execution.private_scope(), &bound[0].name)
                        .expect("completed capture installs its original checked binder");
                    assert_eq!(installed.0.raw(), bound[0].var_id);
                    expected_observation_seen = true;
                }
            }
        }
        assert_eq!(prefix.snapshot().compiler_prefix().next_item(), index + 1);
        assert_eq!(
            tidepool_extract_cmd::extract_spawn_count(),
            submissions_before_effects,
            "native execution must consume the immutable cell without compiler requests"
        );
    }
    if expected_observation.is_some() {
        assert!(
            expected_observation_seen,
            "expected source to execute through the native observation path"
        );
    }
    let work = checked.checked_item(0).unwrap().input_work();
    eprintln!(
        "protected-scale {}",
        serde_json::json!({
            "schema": 1, "prefix": scenario.0, "baseline": scenario.1, "phase": format!("{label}.checked_input_work"),
            "initial_files_written": work.initial_files_written,
            "initial_bytes_written_and_hashed": work.initial_bytes_written_and_hashed,
            "output_files_hashed": work.output_files_hashed, "output_bytes_hashed": work.output_bytes_hashed,
        })
    );
    let private_winners = resident
        .public_visibility_snapshot_in(execution.private_scope())
        .unwrap()
        .bindings;
    let intent = measured(
        resident,
        images,
        scenario,
        &format!("{label}.freeze"),
        None,
        |resident| resident.freeze_private_execution(&execution).unwrap(),
    );
    for id in intent.native_write_ids() {
        let (name, _) = private_winners
            .iter()
            .find(|(_, winner)| winner == id)
            .expect("sealed native write must be a final private winner");
        expected_public_winners.insert(name.clone(), *id);
    }
    let publication = match publication_target {
        ScalePublication::Ephemeral => resident.restage_ephemeral_execution_publication(intent),
        ScalePublication::Durable { owner, .. } => {
            resident.restage_execution_publication(owner.clone(), intent)
        }
    }
    .unwrap();
    let (ticket, certification_ns, stage_ns) = match publication {
        ExecutionPublication::Bindings(base) => {
            let (ticket, stage_ns) = measured_duration(
                resident,
                images,
                scenario,
                &format!("{label}.metadata_stage_file_sync"),
                None,
                |_| base.stage().unwrap(),
            );
            (ticket, None, stage_ns)
        }
        ExecutionPublication::Declarations(base) => {
            let (certified, certification_ns) = measured_duration(
                resident,
                images,
                scenario,
                &format!("{label}.declaration_certification"),
                None,
                |_| base.certify().unwrap(),
            );
            let CertifiedDeclarationPublication::Accepted(accepted) = certified else {
                panic!("actual original declaration/prefix publication was rejected")
            };
            let (ticket, stage_ns) = measured_duration(
                resident,
                images,
                scenario,
                &format!("{label}.metadata_stage_file_sync"),
                None,
                |_| accepted.stage().unwrap(),
            );
            (ticket, Some(certification_ns), stage_ns)
        }
    };
    let recovery_work = ticket.recovery_work();
    let (_, publication_ns) = measured_duration(
        resident,
        images,
        scenario,
        &format!("{label}.publication_rename_directory_sync"),
        None,
        |resident| {
            let result = resident
                .publish_staged_public_manifest(ticket, &PublicationDecision::new())
                .unwrap();
            assert_eq!(
                result,
                match publication_target {
                    ScalePublication::Ephemeral => PublicManifestCommit::Ephemeral,
                    ScalePublication::Durable { .. } => PublicManifestCommit::Durable,
                }
            );
        },
    );
    assert_eq!(
        resident
            .public_visibility_snapshot_in(public)
            .unwrap()
            .bindings,
        expected_public_winners.into_iter().collect::<Vec<_>>(),
        "publication must preserve prior public winners and publish sealed native writes"
    );
    // Retained snapshots and diagnostic reads do not belong to cell latency.
    let elapsed = cell_started.elapsed();
    let inventory = resident.compile_view_in(public).and_then(|view| {
        view.exact_declaration_context()
            .map(|context| context.artifact_view().inventory().metrics())
    });
    if let ScalePublication::Durable { manifest, .. } = publication_target {
        let bytes = std::fs::read(manifest).unwrap();
        let document: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let snapshot = manifest.with_file_name(format!("declarations-{label}.json"));
        std::fs::write(&snapshot, &bytes).unwrap();
        eprintln!(
            "durable-performance {}",
            serde_json::json!({
                "schema": 1, "composition": "durable-publication", "cell": label,
                "prefix": scenario.0, "baseline": scenario.1, "completed": true,
                "elapsed_ns": elapsed.as_nanos(), "certification_ns": certification_ns,
                "metadata_stage_file_sync_ns": stage_ns,
                "publication_rename_directory_sync_ns": publication_ns,
                "manifest_path": snapshot, "manifest_bytes": bytes.len(),
                "manifest_blake3": blake3::hash(&bytes).to_hex().to_string(),
                "manifest_checksum": document.get("checksum"),
                "public_schema": document.get("public_schema"),
                "checksum_encode_bytes": recovery_work.checksum_encode_bytes,
                "recovery_validation_hash_bytes": recovery_work.recovery_validation_hash_bytes,
                "recovery_materialization_hash_bytes": recovery_work.recovery_materialization_hash_bytes,
                "manifest_write_bytes": recovery_work.manifest_write_bytes,
                "artifact_inventory": inventory,
                "inventory_counter_scope": "shared-artifact-inventory-owner",
            })
        );
    }
    eprintln!(
        "protected-scale {}",
        serde_json::json!({
            "schema": 1, "prefix": scenario.0, "baseline": scenario.1,
            "phase": format!("{label}.artifact_inventory"), "inventory": inventory,
        })
    );
    Ok((elapsed, native_targets))
}

fn execute_growth_cell(
    resident: &mut ScaleSession,
    public: ScopeId,
    effects: &TestEffectSurface,
    images: &ImageRegistry,
    scenario: (usize, usize),
    label: &str,
    source: &str,
    declarations: usize,
    publication: &ScalePublication,
    observer: Option<&CellRequestObserver>,
) -> Duration {
    let Some(observer) = observer else {
        return execute_cell(
            resident,
            public,
            effects,
            images,
            scenario,
            label,
            source,
            declarations,
            publication,
        );
    };
    let (elapsed, _) = with_cell_request_capture(observer, "binding_growth", label, source, || {
        Ok(execute_cell(
            resident,
            public,
            effects,
            images,
            scenario,
            label,
            source,
            declarations,
            publication,
        ))
    });
    elapsed.unwrap()
}

fn growing_prefix_with_publication(prefix: usize, baseline: usize, durable: bool) {
    tidepool_testing::eval_harness::require_extract();
    let no_daemon = std::env::var("TIDEPOOL_EXTRACT_NO_DAEMON");
    if durable {
        assert_ne!(
            no_daemon.as_deref(),
            Ok("1"),
            "durable scaling requires the resident compiler"
        );
        let socket = PathBuf::from(
            std::env::var_os(tidepool_extract_cmd::DAEMON_SOCKET_ENV)
                .expect("resident scaling socket"),
        );
        tidepool_extract_cmd::preflight_compiler_daemon(&socket).unwrap();
    } else {
        assert_eq!(
            no_daemon.as_deref(),
            Ok("1"),
            "the historical comparison uses isolated compiler submissions"
        );
    };
    let (root_path, root_guard) = scale_workspace(durable);
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let images = Arc::new(ImageRegistry::new());
    let mut lib = SessionLib::open(SessionId(999), &root_path, ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(effects.include_paths().to_vec());
    let publication = if durable {
        let manifest = root_path.join("declarations.json");
        lib.attach_owned_recovery_graph_v3(&manifest, scale_run_owner(&root_path))
            .unwrap();
        ScalePublication::Durable {
            owner: RecoveryPublicOwner::new(
                &tidepool_repr::ActorPath::parse("root/performance").unwrap(),
                1,
            )
            .unwrap(),
            manifest,
        }
    } else {
        ScalePublication::Ephemeral
    };
    let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
    persistent.set_image_registry(images.clone());
    let public = persistent.mint_scope(ScopeId::ROOT).unwrap();
    let mut resident =
        ResidentSession::from_persistent_for_test(frunk::HNil, QuietOutput, persistent);
    let observer = durable.then(CellRequestObserver::default);
    let scenario = (prefix, baseline);
    if let ScalePublication::Durable { owner, .. } = &publication {
        measured(
            &mut resident,
            &images,
            scenario,
            "durable_initialization",
            None,
            |resident| {
                assert_eq!(
                    resident
                        .initialize_durable_public_scope(owner.clone(), public)
                        .unwrap(),
                    PublicManifestCommit::Durable
                );
            },
        );
    }
    execute_growth_cell(
        &mut resident,
        public,
        &effects,
        &images,
        scenario,
        "foundation",
        include_str!("fixtures/protected-scale-foundation.hs"),
        1,
        &publication,
        observer.as_ref(),
    );
    let original = resident
        .compile_view_in(public)
        .unwrap()
        .exact_declaration_context()
        .unwrap()
        .recovery_products()
        .to_vec();
    if baseline != 0 {
        // GHC tuples permit at most 62 fields; two 50-field items provide
        // 100 real bindings without 100 separately compiled setup items.
        let source = (0..baseline)
            .collect::<Vec<_>>()
            .chunks(50)
            .map(|indices| {
                let names = indices
                    .iter()
                    .map(|index| format!("baseline_{index:04}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let values = indices
                    .iter()
                    .map(|index| format!("({index} :: Int)"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("let ({names}) = ({values})\n")
            })
            .collect::<String>();
        execute_growth_cell(
            &mut resident,
            public,
            &effects,
            &images,
            scenario,
            "baseline",
            &source,
            0,
            &publication,
            observer.as_ref(),
        );
    }
    let mut source = String::new();
    for index in 1..=prefix {
        let previous = if index == 1 {
            "0".to_owned()
        } else {
            format!("scale_value_{:04}", index - 1)
        };
        source.push_str(
            &include_str!("fixtures/protected-scale-bind.hs")
                .replace("SCALE_BINDING", &format!("scale_value_{index:04}"))
                .replace("SCALE_PREVIOUS", &previous),
        );
    }
    if baseline == 0 {
        source.push_str(&format!("scale_value_{prefix:04}\n"));
    } else {
        source.push_str(&format!(
            "scale_value_{prefix:04} + baseline_0000 + (baseline_{:04} - {})\n",
            baseline - 1,
            baseline - 1,
        ));
    }
    execute_growth_cell(
        &mut resident,
        public,
        &effects,
        &images,
        scenario,
        "prefix",
        &source,
        0,
        &publication,
        observer.as_ref(),
    );
    let visible = resident.binding_names_in(public);
    assert_eq!(
        visible
            .iter()
            .filter(|name| name.starts_with("scale_value_"))
            .count(),
        prefix
    );
    assert_eq!(
        visible
            .iter()
            .filter(|name| name.starts_with("baseline_"))
            .count(),
        baseline
    );
    let final_view = resident.compile_view_in(public).unwrap();
    let retained = final_view
        .exact_declaration_context()
        .unwrap()
        .recovery_products();
    for product in &original {
        assert!(
            retained.contains(product),
            "the compiled original product changed during settlement"
        );
    }
    drop(resident);
    drop(root_guard);
}

fn growing_prefix(prefix: usize, baseline: usize) {
    growing_prefix_with_publication(prefix, baseline, false);
}

#[test]
#[ignore = "resident durable scaling attribution; run after the two-cell baseline"]
fn resident_durable_growing_prefix_1_baseline_0() {
    growing_prefix_with_publication(1, 0, true);
}

#[test]
#[ignore = "resident durable scaling attribution; run after the two-cell baseline"]
fn resident_durable_growing_prefix_100_baseline_0() {
    growing_prefix_with_publication(100, 0, true);
}

#[test]
#[ignore = "resident durable scaling attribution; run after the two-cell baseline"]
fn resident_durable_growing_prefix_1_baseline_100() {
    growing_prefix_with_publication(1, 100, true);
}

#[test]
#[ignore = "resident durable scaling attribution; run after the two-cell baseline"]
fn resident_durable_growing_prefix_100_baseline_100() {
    growing_prefix_with_publication(100, 100, true);
}

#[test]
#[ignore = "resident durable scaling attribution; run after the two-cell baseline"]
fn resident_durable_growing_prefix_10_baseline_0() {
    growing_prefix_with_publication(10, 0, true);
}

#[test]
#[ignore = "resident durable scaling attribution; run after the two-cell baseline"]
fn resident_durable_growing_prefix_10_baseline_100() {
    growing_prefix_with_publication(10, 100, true);
}

#[test]
fn protected_growing_prefix_1_baseline_0() {
    growing_prefix(1, 0);
}
#[test]
fn protected_growing_prefix_10_baseline_0() {
    growing_prefix(10, 0);
}
#[test]
fn protected_growing_prefix_100_baseline_0() {
    growing_prefix(100, 0);
}
#[test]
fn protected_growing_prefix_1_baseline_100() {
    growing_prefix(1, 100);
}
#[test]
fn protected_growing_prefix_1_baseline_2() {
    growing_prefix(1, 2);
}
#[test]
fn protected_growing_prefix_10_baseline_100() {
    growing_prefix(10, 100);
}
#[test]
fn protected_growing_prefix_100_baseline_100() {
    growing_prefix(100, 100);
}

// The source action checks its integer before returning the opaque capture
// thunk. Capture completion does not render or force that thunk in Rust.
fn guarded_integer_capture_source(expression: &str, expected: i64) -> String {
    include_str!("fixtures/guarded-integer-capture.hs")
        .replace("__EXPRESSION__", expression)
        .replace("__EXPECTED__", &expected.to_string())
}

#[test]
fn guarded_integer_capture_rejects_wrong_native_result() {
    tidepool_testing::eval_harness::require_extract();
    let root = tempfile::tempdir().unwrap();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let images = Arc::new(ImageRegistry::new());
    let lib = SessionLib::open(
        SessionId(1002),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(effects.include_paths().to_vec());
    let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
    persistent.set_image_registry(images.clone());
    let public = persistent.mint_scope(ScopeId::ROOT).unwrap();
    let mut resident =
        ResidentSession::from_persistent_for_test(frunk::HNil, QuietOutput, persistent);
    let before = resident
        .public_visibility_snapshot_in(public)
        .unwrap()
        .bindings;
    let source = guarded_integer_capture_source("41", 42);
    let error = try_execute_cell_with_template_imports_expectation(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "wrong_integer_guard",
        &source,
        0,
        &ScalePublication::Ephemeral,
        AuthorityChecks::Configured,
        &SourceImports::new(),
        Some(42),
    )
    .expect_err("native 41 must fail its authored 42 guard before capture");
    let ResidentError::Prepared(PreparedRuntimeError::Run(
        tidepool_codegen::prepared_program::ExecutionError::Runtime(failure),
    )) = error
    else {
        panic!("wrong integer guard did not fail during native execution: {error:?}");
    };
    assert!(
        matches!(
            failure.cause,
            tidepool_codegen::host_fns::RuntimeError::RaisedException
                | tidepool_codegen::host_fns::RuntimeError::RaisedExceptionMessage(_)
        ),
        "wrong integer guard did not raise its language exception: {failure:?}"
    );
    assert_eq!(
        resident
            .public_visibility_snapshot_in(public)
            .unwrap()
            .bindings,
        before,
        "failed integer guard must publish no captured binding"
    );
}

/// Compiler/native attribution only; the packaged Engine/Store gate is separate.
fn resident_capture_cells(count: usize, durable: bool) {
    tidepool_testing::eval_harness::require_extract();
    assert_ne!(
        std::env::var("TIDEPOOL_EXTRACT_NO_DAEMON").as_deref(),
        Ok("1")
    );
    let socket = PathBuf::from(
        std::env::var_os(tidepool_extract_cmd::DAEMON_SOCKET_ENV)
            .expect("resident measurement requires its owned compiler socket"),
    );
    let identity = tidepool_extract_cmd::preflight_compiler_daemon(&socket)
        .expect("resident measurement cannot use direct fallback");
    let (root_path, root_guard) = scale_workspace(durable);
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let images = Arc::new(ImageRegistry::new());
    let mut lib = SessionLib::open(SessionId(1000), &root_path, ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(effects.include_paths().to_vec());
    let publication = if durable {
        let manifest = root_path.join("declarations.json");
        lib.attach_owned_recovery_graph_v3(&manifest, scale_run_owner(&root_path))
            .unwrap();
        ScalePublication::Durable {
            owner: RecoveryPublicOwner::new(
                &tidepool_repr::ActorPath::parse("root/performance").unwrap(),
                1,
            )
            .unwrap(),
            manifest,
        }
    } else {
        ScalePublication::Ephemeral
    };
    let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
    persistent.set_image_registry(images.clone());
    let public = persistent.mint_scope(ScopeId::ROOT).unwrap();
    let mut resident =
        ResidentSession::from_persistent_for_test(frunk::HNil, QuietOutput, persistent);
    let observer = CellRequestObserver::default();
    if let ScalePublication::Durable { owner, .. } = &publication {
        measured(
            &mut resident,
            &images,
            (0, 0),
            "durable_initialization",
            None,
            |resident| {
                assert_eq!(
                    resident
                        .initialize_durable_public_scope(owner.clone(), public)
                        .unwrap(),
                    PublicManifestCommit::Durable
                );
            },
        );
        execute_cell(
            &mut resident,
            public,
            &effects,
            &images,
            (0, 0),
            "foundation",
            include_str!("fixtures/protected-scale-foundation.hs"),
            1,
            &publication,
        );
    }
    // The observer is scoped to each real source submission and associates its
    // physical requests with the one sequential active cell.
    let warm_source = guarded_integer_capture_source("42", 42);
    let (warm_result, warm_capture) =
        with_cell_request_capture(&observer, "warmup", "warmup", &warm_source, || {
            try_execute_cell_with_template_imports_expectation(
                &mut resident,
                public,
                &effects,
                &images,
                (0, 0),
                "warmup",
                &warm_source,
                0,
                &publication,
                AuthorityChecks::Configured,
                &SourceImports::new(),
                Some(42),
            )
        });
    let warm_elapsed = warm_result.unwrap().0;
    eprintln!(
        "resident-performance-warmup {}",
        serde_json::json!({
            "schema": 1, "composition": "private-session", "kind": "warmup_cell",
            "index": warm_capture.index, "label": warm_capture.label,
            "elapsed_ns": warm_elapsed.as_nanos(), "completed": true, "captured": true,
            "workload": "guarded-integer-capture", "expected_result": 42,
            "value_check": "native-integer-guard-before-opaque-capture",
            "source_blake3": blake3::hash(warm_source.as_bytes()).to_hex().to_string(),
            "source_sha256": warm_capture.source_sha256,
            "source_path": warm_capture.source_path,
            "compiler_requests": warm_capture.compiler_requests,
            "endpoint": identity.to_hex(), "producer": identity.producer_hex(),
        })
    );
    for index in 0..count {
        let source = guarded_integer_capture_source(
            &format!("{index} + ({})", 42_i64 - i64::try_from(index).unwrap()),
            42,
        );
        assert_eq!(
            tidepool_extract_cmd::preflight_compiler_daemon(&socket).unwrap(),
            identity
        );
        let label = format!("resident_cell_{index}");
        let (result, captured) =
            with_cell_request_capture(&observer, "measured", &label, &source, || {
                try_execute_cell_with_template_imports_expectation(
                    &mut resident,
                    public,
                    &effects,
                    &images,
                    (0, 0),
                    &label,
                    &source,
                    0,
                    &publication,
                    AuthorityChecks::Configured,
                    &SourceImports::new(),
                    Some(42),
                )
            });
        let elapsed = result.unwrap().0;
        assert_eq!(
            tidepool_extract_cmd::preflight_compiler_daemon(&socket).unwrap(),
            identity
        );
        eprintln!(
            "resident-performance {}",
            serde_json::json!({
                "schema": 1, "composition": "private-session", "kind": "warm_cell",
                "index": index, "label": captured.label,
                "elapsed_ns": elapsed.as_nanos(), "completed": true, "captured": true,
                "workload": "guarded-integer-addition-capture", "expected_result": 42,
                "value_check": "native-integer-guard-before-opaque-capture",
                "source_blake3": blake3::hash(source.as_bytes()).to_hex().to_string(),
                "source_sha256": captured.source_sha256,
                "source_path": captured.source_path,
                "compiler_requests": captured.compiler_requests,
                "endpoint": identity.to_hex(), "producer": identity.producer_hex(),
            })
        );
    }
    drop(resident);
    drop(root_guard);
}

#[test]
#[ignore = "requires an admitted real resident compiler and 50 compiled capture cells"]
fn resident_warm_capture_cells_50() {
    resident_capture_cells(50, false);
}

#[test]
#[ignore = "requires an admitted real resident compiler for a bounded baseline"]
fn resident_capture_cells_2_baseline() {
    resident_capture_cells(2, false);
}

#[test]
#[ignore = "requires an admitted real resident compiler and retains its durable workspace"]
fn resident_durable_capture_cells_2_baseline() {
    resident_capture_cells(2, true);
}

fn simple_cell_vertical(
    label: &str,
    source: &str,
    declarations: usize,
    authority_checks: AuthorityChecks,
) -> Vec<String> {
    tidepool_testing::eval_harness::require_extract();
    let root = tempfile::tempdir().unwrap();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let images = Arc::new(ImageRegistry::new());
    let lib = SessionLib::open(
        SessionId(1001),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(effects.include_paths().to_vec());
    let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
    persistent.set_image_registry(images.clone());
    let public = persistent.mint_scope(ScopeId::ROOT).unwrap();
    let mut resident =
        ResidentSession::from_persistent_for_test(frunk::HNil, QuietOutput, persistent);
    execute_cell_with_authority_checks(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        label,
        source,
        declarations,
        &ScalePublication::Ephemeral,
        authority_checks,
    );
    resident.binding_names_in(public)
}

#[test]
fn complete_cell_consumes_native_items_without_compiler_requests() {
    simple_cell_vertical(
        "complete_cell",
        include_str!("fixtures/compiled-cell-simple.hs"),
        0,
        AuthorityChecks::RefusalBranches,
    );
}

#[test]
fn following_declaration_retains_original_native_binding_inventory() {
    tidepool_testing::eval_harness::require_extract();
    let root = tempfile::tempdir().unwrap();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let images = Arc::new(ImageRegistry::new());
    let lib = SessionLib::open(
        SessionId(1004),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(effects.include_paths().to_vec());
    let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
    persistent.set_image_registry(images.clone());
    let public = persistent.mint_scope(ScopeId::ROOT).unwrap();
    let mut resident =
        ResidentSession::from_persistent_for_test(frunk::HNil, QuietOutput, persistent);
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "original_binding",
        "x <- pure (1 :: Int)",
        0,
        &ScalePublication::Ephemeral,
    );
    let original = resident.current_binding_in(public, "x").unwrap();
    assert_eq!(
        original.1,
        tidepool_repr::SessionModule::val(tidepool_repr::Generation(1))
    );
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "following_declaration",
        include_str!("fixtures/compiled-cell-native-binding-declaration.hs"),
        1,
        &ScalePublication::Ephemeral,
    );
    assert_eq!(resident.current_binding_in(public, "x").unwrap(), original);
    std::fs::write(
        root.path().join("HiddenValSupport.hs"),
        include_str!("fixtures/checked-native-support.hs"),
    )
    .unwrap();
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "following_support_declaration",
        include_str!("fixtures/compiled-cell-native-binding-support.hs"),
        2,
        &ScalePublication::Ephemeral,
    );
    assert_eq!(resident.current_binding_in(public, "x").unwrap(), original);
    let public_view = resident.compile_view_in(public).unwrap();
    let context = public_view.exact_declaration_context().unwrap();
    assert!(context
        .artifact_view()
        .descriptors()
        .iter()
        .any(|descriptor| descriptor.owner.module == original.1.module_name()));
    assert!(!context.lexical_graph().iter().any(|node| {
        node.owner.module == original.1.module_name()
            || node
                .imports
                .iter()
                .any(|owner| owner.module == original.1.module_name())
    }));
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "following_support_consumer",
        "dependentThroughSupport",
        0,
        &ScalePublication::Ephemeral,
    );
    assert_eq!(resident.current_binding_in(public, "x").unwrap(), original);
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "support_rebind_x",
        "let x = (2 :: Int)",
        0,
        &ScalePublication::Ephemeral,
    );
    assert_ne!(resident.current_binding_in(public, "x").unwrap(), original);
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "support_original_after_rebind",
        "dependentThroughSupport",
        0,
        &ScalePublication::Ephemeral,
    );
}

#[test]
fn following_declaration_publishes_current_source_selected_originals() {
    let subscriber = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "tidepool_toolchain::planned_source_admission=debug",
        ))
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);
    tidepool_testing::eval_harness::require_extract();
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("CheckedHomeValue.hs"),
        include_str!("fixtures/checked-home-value.hs"),
    )
    .unwrap();
    std::fs::write(
        root.path().join("UnrelatedHomeValue.hs"),
        include_str!("fixtures/unrelated-home-value.hs"),
    )
    .unwrap();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let images = Arc::new(ImageRegistry::new());
    let lib = SessionLib::open(
        SessionId(1006),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(effects.include_paths().to_vec());
    let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
    persistent.set_image_registry(images.clone());
    let public = persistent.mint_scope(ScopeId::ROOT).unwrap();
    let mut resident =
        ResidentSession::from_persistent_for_test(frunk::HNil, QuietOutput, persistent);
    try_execute_cell_with_template_imports(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "retained_hidden_home",
        "let home = CheckedHomeValue.homeValue",
        0,
        &ScalePublication::Ephemeral,
        AuthorityChecks::Configured,
        &SourceImports::from_specs(["qualified CheckedHomeValue"]),
    )
    .unwrap();
    try_execute_cell_with_template_imports(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "retained_unrelated_home",
        "let unrelated = UnrelatedHomeValue.unrelatedValue",
        0,
        &ScalePublication::Ephemeral,
        AuthorityChecks::Configured,
        &SourceImports::from_specs(["qualified UnrelatedHomeValue"]),
    )
    .unwrap();
    assert!(resident
        .compile_view_in(public)
        .unwrap()
        .exact_declaration_context()
        .is_none_or(|context| !context
            .lexical_graph()
            .iter()
            .any(|node| node.owner.module == "CheckedHomeValue")));
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "source_selected_declaration",
        include_str!("fixtures/compiled-cell-source-selected-declaration.hs"),
        2,
        &ScalePublication::Ephemeral,
    );
    let view = resident.compile_view_in(public).unwrap();
    let context = view.exact_declaration_context().unwrap();
    assert!(context
        .lexical_graph()
        .iter()
        .any(|node| node.owner.module == "CheckedHomeValue"));
    assert!(!context
        .lexical_graph()
        .iter()
        .any(|node| node.owner.module == "UnrelatedHomeValue"));
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "source_selected_consumer",
        "sourceSelected home",
        0,
        &ScalePublication::Ephemeral,
    );
    assert!(!resident
        .compile_view_in(public)
        .unwrap()
        .exact_declaration_context()
        .unwrap()
        .lexical_graph()
        .iter()
        .any(|node| node.owner.module == "UnrelatedHomeValue"));
}

#[test]
fn same_cell_authored_import_retains_completed_quasiquote_support() {
    tidepool_testing::eval_harness::require_extract();
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("SameCellImportSupport.hs"),
        include_str!("fixtures/same-cell-import-support.hs"),
    )
    .unwrap();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let images = Arc::new(ImageRegistry::new());
    let lib = SessionLib::open(
        SessionId(1008),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(effects.include_paths().to_vec());
    let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
    persistent.set_image_registry(images.clone());
    let public = persistent.mint_scope(ScopeId::ROOT).unwrap();
    let mut resident =
        ResidentSession::from_persistent_for_test(frunk::HNil, QuietOutput, persistent);
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "same_cell_authored_import",
        include_str!("fixtures/compiled-cell-same-source-import.hs"),
        2,
        &ScalePublication::Ephemeral,
    );
    assert!(resident
        .compile_view_in(public)
        .unwrap()
        .exact_declaration_context()
        .unwrap()
        .lexical_graph()
        .iter()
        .any(|node| node.owner.module == "SameCellImportSupport"));
    let public_bindings = resident.workbench_bindings_in(public);
    let declared = public_bindings
        .iter()
        .filter(|binding| binding.name == "sameCellOriginal")
        .collect::<Vec<_>>();
    let [declared] = declared.as_slice() else {
        panic!("published source must have exactly one declaration winner");
    };
    assert_eq!(
        declared.kind,
        crate::session::WorkbenchBindingKind::Declaration
    );
    assert!(declared.defining_generation().is_some());
    assert!(!resident.binding_names_in(public).contains(&declared.name));
    let original_support = resident
        .compile_view_in(public)
        .unwrap()
        .exact_declaration_context()
        .unwrap()
        .recovery_products()
        .into_iter()
        .find(|product| product.owner().module == "SameCellImportSupport")
        .expect("completed source import must retain its original native owner")
        .owner()
        .clone();
    std::fs::remove_file(root.path().join("SameCellImportSupport.hs")).unwrap();
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "published_same_cell_original",
        "if sameCellOriginal == 42 then pure () else error \"published original returned the wrong value\"",
        0,
        &ScalePublication::Ephemeral,
    );
    assert!(resident
        .compile_view_in(public)
        .unwrap()
        .exact_declaration_context()
        .unwrap()
        .recovery_products()
        .iter()
        .any(|product| product.owner() == &original_support));
}

#[test]
fn following_cells_reprove_template_imports_of_retained_rich_originals() {
    tidepool_testing::eval_harness::require_extract();
    let root = tempfile::tempdir().unwrap();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let images = Arc::new(ImageRegistry::new());
    let lib = SessionLib::open(
        SessionId(1007),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(effects.include_paths().to_vec());
    let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
    persistent.set_image_registry(images.clone());
    let public = persistent.mint_scope(ScopeId::ROOT).unwrap();
    let mut resident =
        ResidentSession::from_persistent_for_test(frunk::HNil, QuietOutput, persistent);
    let imports = SourceImports::from_specs(["qualified Tidepool.Aeson as Aeson"]);
    for (label, source) in [
        ("original_rich_value", "let originalJSON = Aeson.object []"),
        (
            "following_rich_value",
            "let nextJSON = Aeson.object []\n(42 :: Int)",
        ),
        ("following_rich_consumer", "(42 :: Int)"),
    ] {
        try_execute_cell_with_template_imports(
            &mut resident,
            public,
            &effects,
            &images,
            (0, 0),
            label,
            source,
            0,
            &ScalePublication::Ephemeral,
            AuthorityChecks::Configured,
            &imports,
        )
        .unwrap();
        assert!(resident
            .compile_view_in(public)
            .unwrap()
            .exact_declaration_context()
            .is_none_or(|context| !context
                .lexical_graph()
                .iter()
                .any(|node| node.owner.module == "Tidepool.Aeson")));
    }
    try_execute_cell_with_template_imports(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "following_without_rich_import",
        "let independent = 42",
        0,
        &ScalePublication::Ephemeral,
        AuthorityChecks::Configured,
        &SourceImports::default(),
    )
    .unwrap();
    assert!(resident
        .compile_view_in(public)
        .unwrap()
        .exact_declaration_context()
        .is_none_or(|context| !context
            .lexical_graph()
            .iter()
            .any(|node| node.owner.module == "Tidepool.Aeson")));
}

#[test]
fn following_cells_preserve_quoted_template_original_without_lexical_promotion() {
    tidepool_testing::eval_harness::require_extract();
    let root = tempfile::tempdir().unwrap();
    let support = root.path().join("QuotedTemplateSupport.hs");
    std::fs::write(
        &support,
        include_str!("fixtures/quoted-template-support.hs"),
    )
    .unwrap();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let images = Arc::new(ImageRegistry::new());
    let lib = SessionLib::open(
        SessionId(1017),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(effects.include_paths().to_vec());
    let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
    persistent.set_image_registry(images.clone());
    let public = persistent.mint_scope(ScopeId::ROOT).unwrap();
    let mut resident =
        ResidentSession::from_persistent_for_test(frunk::HNil, QuietOutput, persistent);
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "before_quoted_template_value",
        "let smokeValue = (6 * 7 :: Int)",
        0,
        &ScalePublication::Ephemeral,
    );
    let imports = SourceImports::from_specs(["QuotedTemplateSupport"]);
    try_execute_cell_with_template_imports(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "quoted_template_value",
        "let retainedTask = taskValue 41",
        0,
        &ScalePublication::Ephemeral,
        AuthorityChecks::Configured,
        &imports,
    )
    .unwrap();
    let context = resident
        .compile_view_in(public)
        .unwrap()
        .exact_declaration_context()
        .cloned()
        .expect("published binding retains its exact original support");
    eprintln!(
        "quoted-template retained inventory: {:?}; lexical: {:?}",
        context.artifact_view().descriptors(),
        context.lexical_graph(),
    );
    let owner = context
        .recovery_products()
        .into_iter()
        .find(|product| product.owner().module == "QuotedTemplateSupport")
        .expect("OPAQUE helper must retain the defining native original")
        .owner()
        .clone();
    eprintln!("quoted-template retained helper original: {owner:?}");
    assert!(context
        .lexical_graph()
        .iter()
        .all(|node| node.owner.module != "QuotedTemplateSupport"));
    for (label, expression, expected) in [
        ("unrelated_cell_after_quoted_template", "smokeValue + 1", 43),
        ("retained_quoted_template_value", "retainedTask", 42),
    ] {
        let view = resident.compile_view_in(public).unwrap();
        eprintln!(
            "quoted-template {label} current template: {}",
            resident_cell_check_template(
                effects.preamble(),
                effects.row(),
                &view.turn_imports(&imports),
            ),
        );
        try_execute_cell_with_template_imports_expectation(
            &mut resident,
            public,
            &effects,
            &images,
            (0, 0),
            label,
            &guarded_integer_capture_source(expression, expected),
            0,
            &ScalePublication::Ephemeral,
            AuthorityChecks::Configured,
            &imports,
            Some(expected),
        )
        .unwrap();
    }
    std::fs::remove_file(&support).unwrap();
    try_execute_cell_with_template_imports_expectation(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "source_absent_quoted_template_original",
        &guarded_integer_capture_source("taskValue 41", 42),
        0,
        &ScalePublication::Ephemeral,
        AuthorityChecks::Configured,
        &imports,
        Some(42),
    )
    .unwrap();
    let context = resident
        .compile_view_in(public)
        .unwrap()
        .exact_declaration_context()
        .cloned()
        .unwrap();
    assert!(context
        .lexical_graph()
        .iter()
        .all(|node| node.owner.module != "QuotedTemplateSupport"));
    assert!(context
        .recovery_products()
        .iter()
        .any(|product| product.owner() == &owner));
}

#[test]
fn late_record_selector_replaces_earlier_cell_value() {
    let names = simple_cell_vertical(
        "record_selector",
        include_str!("fixtures/compiled-cell-record-selector.hs"),
        1,
        AuthorityChecks::Configured,
    );
    assert!(
        !names.iter().any(|name| name == "f"),
        "old heap f must not survive its exact declaration selector"
    );
}

#[test]
#[ignore = "local fixity guard remains until production prepared-cell native8 passes"]
fn complete_cell_preserves_exact_local_fixity() {
    simple_cell_vertical(
        "local_fixity",
        include_str!("fixtures/compiled-cell-local-fixity.hs"),
        0,
        AuthorityChecks::Configured,
    );
}

fn persisted_root_state(
    manifest: &Path,
    owner: &RecoveryPublicOwner,
) -> crate::session::recovery::RecoveryNodeState {
    let graph: crate::session::recovery::RecoveryGraph =
        serde_json::from_slice(&std::fs::read(manifest).unwrap()).unwrap();
    let root = graph
        .public_surfaces()
        .find(|surface| &surface.owner == owner)
        .unwrap()
        .declaration_root
        .unwrap();
    graph.node(root).unwrap().state.clone()
}

#[test]
#[ignore = "requires an admitted real compiler, retained durable workspace, and fresh native test process"]
fn durable_mixed_originals_recover_independent_native_entry() {
    tidepool_testing::eval_harness::require_extract();
    let child_root = std::env::var_os("TIDEPOOL_RECOVERY_CHILD_WORKSPACE").map(PathBuf::from);
    let (root_path, root_guard) = if let Some(root) = &child_root {
        (root.clone(), None)
    } else {
        scale_workspace(true)
    };
    let manifest = root_path.join("declarations.json");
    let owner = RecoveryPublicOwner::new(
        &tidepool_repr::ActorPath::parse("root/performance").unwrap(),
        1,
    )
    .unwrap();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let images = Arc::new(ImageRegistry::new());
    let source_root = if child_root.is_some() {
        let source = root_path.join("fresh-process-source");
        std::fs::create_dir(&source).unwrap();
        source
    } else {
        root_path.clone()
    };
    let mut lib = SessionLib::open(
        SessionId(if child_root.is_some() { 1003 } else { 1002 }),
        &source_root,
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(effects.include_paths().to_vec());
    lib.attach_owned_recovery_graph_v3(&manifest, scale_run_owner(&root_path))
        .unwrap();
    let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
    persistent.set_image_registry(images.clone());
    let public = if child_root.is_some() {
        persistent.recover_public_scope(&owner).unwrap()
    } else {
        persistent.mint_scope(ScopeId::ROOT).unwrap()
    };
    let mut resident =
        ResidentSession::from_persistent_for_test(frunk::HNil, QuietOutput, persistent);
    let publication = ScalePublication::Durable {
        owner: owner.clone(),
        manifest: manifest.clone(),
    };
    if child_root.is_some() {
        assert!(!resident
            .binding_names_in(public)
            .iter()
            .any(|name| name == "x"));
        eprintln!("durable-recovery fresh_process_pid={}", std::process::id());
        // One original owns both entries. Hydration must retain its complete
        // typed interface while native demand enforces the exact old x lease.
        execute_cell(
            &mut resident,
            public,
            &effects,
            &images,
            (0, 0),
            "recovered_independent",
            "independent",
            0,
            &publication,
        );
        let missing_before = demand_missing_retained(
            &mut resident,
            public,
            &effects,
            &images,
            "lost_dependent_before_rebind",
            &publication,
        );
        execute_cell(
            &mut resident,
            public,
            &effects,
            &images,
            (0, 0),
            "rebind_x",
            "let x = (2 :: Int)",
            0,
            &publication,
        );
        assert!(resident
            .binding_names_in(public)
            .iter()
            .any(|name| name == "x"));
        let missing_after = demand_missing_retained(
            &mut resident,
            public,
            &effects,
            &images,
            "lost_dependent_after_rebind",
            &publication,
        );
        assert_eq!(
            missing_after, missing_before,
            "same-spelled x must not satisfy the original's exact old Val import"
        );
        return;
    }
    assert_eq!(
        resident
            .initialize_durable_public_scope(owner.clone(), public)
            .unwrap(),
        PublicManifestCommit::Durable
    );
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "live_x",
        "x <- pure (1 :: Int)",
        0,
        &publication,
    );
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "independent_original",
        include_str!("fixtures/compiled-cell-independent-original.hs"),
        1,
        &publication,
    );
    assert!(matches!(
        persisted_root_state(&manifest, &owner),
        crate::session::recovery::RecoveryNodeState::ExactArtifactClosure
    ));
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "mixed_original",
        include_str!("fixtures/compiled-cell-mixed-original.hs"),
        1,
        &publication,
    );
    assert!(matches!(
        persisted_root_state(&manifest, &owner),
        crate::session::recovery::RecoveryNodeState::LiveValueDependency { .. }
    ));
    drop(resident);
    // Execute the already-built native test binary in a fresh process. The
    // battery still owns the one compiler daemon and configured endpoint.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["session::turn::scaling_tests::durable_mixed_originals_recover_independent_native_entry", "--ignored", "--exact", "--nocapture"])
        .env("TIDEPOOL_RECOVERY_CHILD_WORKSPACE", &root_path).output().unwrap();
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    println!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "fresh process must execute the mixed original independent entry and refuse its exact lost dependency"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed;"),
        "fresh process must select exactly its one test"
    );
    drop(root_guard);
}

/// Any compile, authority, source, or unrelated runtime failure fails this test.
/// Only the native resolver's exact missing retained owner counts as refusal.
fn demand_missing_retained(
    resident: &mut ScaleSession,
    public: ScopeId,
    effects: &TestEffectSurface,
    images: &ImageRegistry,
    label: &str,
    publication: &ScalePublication,
) -> (tidepool_repr::execution_schema::SymbolIdentity, u64) {
    let error = try_execute_cell_with_authority_checks(
        resident,
        public,
        effects,
        images,
        (0, 0),
        label,
        "dependent",
        0,
        publication,
        AuthorityChecks::Configured,
    )
    .expect_err("demanding the old retained x must refuse native installation");
    let ResidentError::Prepared(PreparedRuntimeError::MissingRetainedCertifiedOwner {
        identity,
        generation,
    }) = error
    else {
        panic!("expected exact retained native refusal, got {error:?}");
    };
    eprintln!("durable-recovery exact_missing_retained={identity:?} generation={generation}");
    (identity, generation)
}

/// Exercise Main's actual checked/native pair with a nonempty admitted retained
/// policy. The generated targets stay fresh; an independent source dependency
/// is finalized once and reused by the next phase in the same transaction.
#[test]
#[serial_test::serial]
fn checked_cell_retained_policy_reuses_finalized_dependencies() {
    use super::tests::TestEnvGuard;
    use tidepool_extract_cmd::ExtractRequest;

    tidepool_testing::eval_harness::require_extract();
    let _daemon = TestEnvGuard::unset("TIDEPOOL_EXTRACT_DAEMON_SOCKET");
    let _timing = TestEnvGuard::set("TIDEPOOL_TIMING", "1");
    for declaration in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let _cache = TestEnvGuard::set("TIDEPOOL_COMPILE_CACHE_DIR", cache.path());
        let effects = TestEffectSurface::minimal(&[]).unwrap();
        let images = Arc::new(ImageRegistry::new());
        let lib = SessionLib::open(
            SessionId(1018),
            root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(effects.include_paths().to_vec());
        let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
        persistent.set_image_registry(images.clone());
        let public = persistent.mint_scope(ScopeId::ROOT).unwrap();
        let mut resident =
            ResidentSession::from_persistent_for_test(frunk::HNil, QuietOutput, persistent);
        execute_cell(
            &mut resident,
            public,
            &effects,
            &images,
            (0, 0),
            "memo_policy_seed",
            "memoSeed <- pure (41 :: Int)",
            0,
            &ScalePublication::Ephemeral,
        );
        assert!(resident.current_binding_in(public, "memoSeed").is_some());
        std::fs::write(
            root.path().join("SessionBodyDemandSupport.hs"),
            include_str!("fixtures/session-body-demand-support.hs"),
        )
        .unwrap();
        let imports = SourceImports::from_specs(["qualified SessionBodyDemandSupport"]);
        let diagnostics = tempfile::tempdir().unwrap();
        let _capture = TestEnvGuard::set("TIDEPOOL_TEST_DIAGNOSTIC_SCOPE", "1");
        let _root = TestEnvGuard::set("TIDEPOOL_TEST_ARTIFACT_ROOT", diagnostics.path());
        let source = if declaration {
            "memoDeclared :: Int\nmemoDeclared = SessionBodyDemandSupport.retainedFunction memoSeed"
        } else {
            "memoChecked <- if SessionBodyDemandSupport.retainedFunction memoSeed == 42 then pure () else error \"checked/native value mismatch\""
        };
        try_execute_cell_with_template_imports(
            &mut resident,
            public,
            &effects,
            &images,
            (0, 0),
            if declaration {
                "memo_policy_declaration"
            } else {
                "memo_policy_native"
            },
            source,
            usize::from(declaration),
            &ScalePublication::Ephemeral,
            AuthorityChecks::Configured,
            &imports,
        )
        .expect("the admitted checked/native pair must preserve its retained value");
        if !declaration {
            assert!(resident.current_binding_in(public, "memoChecked").is_some());
        }
        let transactions = std::fs::read_dir(diagnostics.path().join("compiler-transactions"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        let relevant = transactions
            .iter()
            .filter_map(|directory| {
                let stderr = std::fs::read_to_string(directory.join("compiler.stderr")).ok()?;
                stderr
                    .contains("module=SessionBodyDemandSupport")
                    .then_some((directory, stderr))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            relevant.len(),
            1,
            "one physical whole-cell compiler request"
        );
        let (directory, stderr) = &relevant[0];
        let request =
            ExtractRequest::decode(&std::fs::read(directory.join("compiler-request.bin")).unwrap())
                .unwrap();
        assert!(
            request.retained_generations().keys().any(|identity| {
                identity.module.starts_with("Tidepool.Session.Val.")
                    && identity.occurrence == "memoSeed"
            }),
            "the physical request must carry the genuine retained home value"
        );
        for phase in ["frontend", "finalization"] {
            let marker = format!("tidepool-canonical-{phase} module=SessionBodyDemandSupport");
            assert_eq!(
                stderr.lines().filter(|line| *line == marker).count(),
                1,
                "shared source {phase} repeated in declaration={declaration}: {stderr}"
            );
        }
        assert!(
            stderr.lines().any(|line| {
                line.strip_prefix("tidepool-count name=transaction_reused_source_products count=")
                    .and_then(|rest| rest.split_whitespace().next())
                    .and_then(|count| count.parse::<usize>().ok())
                    .is_some_and(|count| count > 0)
            }),
            "native compilation must consume validated canonical products: {stderr}"
        );
        assert!(
            stderr.lines().any(|line| {
                line.starts_with("tidepool-checked module=") && line.ends_with(" target=True")
            }),
            "checking still owns its fresh generated target: {stderr}"
        );
        if declaration {
            execute_cell(
                &mut resident,
                public,
                &effects,
                &images,
                (0, 0),
                "memo_policy_declaration_value",
                "memoDeclarationChecked <- if memoDeclared == 42 then pure () else error \"retained declaration value mismatch\"",
                0,
                &ScalePublication::Ephemeral,
            );
            assert!(resident
                .current_binding_in(public, "memoDeclarationChecked")
                .is_some());
        }
    }
}

/// Real retained values and a callable cross the checked-cell compiler boundary.
/// Rebinding changes the public winner while previously admitted declarations
/// continue to demand the exact original native owners.
#[test]
fn checked_cell_retained_imports_preserve_value_callable_and_generation() {
    tidepool_testing::eval_harness::require_extract();
    let root = tempfile::tempdir().unwrap();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let images = Arc::new(ImageRegistry::new());
    let lib = SessionLib::open(
        SessionId(1010),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(effects.include_paths().to_vec());
    let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
    persistent.set_image_registry(images.clone());
    let public = persistent.mint_scope(ScopeId::ROOT).unwrap();
    let mut resident =
        ResidentSession::from_persistent_for_test(frunk::HNil, QuietOutput, persistent);
    std::fs::write(
        root.path().join("SessionBodyDemandSupport.hs"),
        include_str!("fixtures/session-body-demand-support.hs"),
    )
    .unwrap();
    let support_import = SourceImports::from_specs(["qualified SessionBodyDemandSupport"]);
    try_execute_cell_with_template_imports(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "session_body_demand_unused_import",
        "pure (0 :: Int)",
        0,
        &ScalePublication::Ephemeral,
        AuthorityChecks::Configured,
        &support_import,
    )
    .expect("an unused selected source owner still typechecks");
    try_execute_cell_with_template_imports(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "session_body_demand_later_call",
        "if SessionBodyDemandSupport.retainedFunction 41 == 42 then pure () else error \"later source body returned the wrong value\"",
        0,
        &ScalePublication::Ephemeral,
        AuthorityChecks::Configured,
        &support_import,
    )
    .expect("a later checked cell prepares and executes the previously unused source body");
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "retained_producer",
        include_str!("fixtures/retained-import-producer.hs"),
        0,
        &ScalePublication::Ephemeral,
    );
    let value_owner = resident
        .current_binding_in(public, "producerValue")
        .unwrap();
    let callable_owner = resident.current_binding_in(public, "producerFn").unwrap();
    assert_ne!(value_owner.0, callable_owner.0);
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "retained_consumer_declaration",
        include_str!("fixtures/retained-import-consumer.hs"),
        1,
        &ScalePublication::Ephemeral,
    );

    // The producer fixture returns this exact list. Its callable adds the
    // list length, so applying it to that length yields 3 + 3.
    let expected_value = [1_i64, 2, 3];
    let expected_result = 6_i64;
    let check_original = format!(
        "if consumerValue == {expected_value:?} && consumerResult == {expected_result} then pure () else error \"retained import oracle mismatch\""
    );
    let (_, targets) = try_execute_cell_with_template_imports(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "retained_original_consumer",
        &check_original,
        0,
        &ScalePublication::Ephemeral,
        AuthorityChecks::Configured,
        &SourceImports::new(),
    )
    .unwrap();
    assert_authentic_retained_link_rejects_wrong_generation(&targets);

    let retained = resident.prepared_retained();
    for owner in [&value_owner, &callable_owner] {
        assert!(retained.iter().any(|(identity, generation)| identity.module
            == owner.1.module_name()
            && *generation == owner.1.gen().0));
    }
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "retained_replacement",
        include_str!("fixtures/retained-import-replacement.hs"),
        0,
        &ScalePublication::Ephemeral,
    );
    assert_ne!(
        resident
            .current_binding_in(public, "producerValue")
            .unwrap(),
        value_owner
    );
    assert_ne!(
        resident.current_binding_in(public, "producerFn").unwrap(),
        callable_owner
    );
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "retained_original_after_rebind",
        &check_original,
        0,
        &ScalePublication::Ephemeral,
    );
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "retained_current_after_rebind",
        "if producerValue == [10,20] && producerFn (length producerValue) == 102 then pure () else error \"current import mismatch\"",
        0,
        &ScalePublication::Ephemeral,
    );
    let roots_before = resident.persistent_roots_count();
    let retirement = resident.retire_scope(public);
    assert!(retirement.bindings_retired >= 4);
    assert!(retirement.roots_released > 0);
    assert_eq!(
        resident.persistent_roots_count(),
        roots_before - retirement.roots_released
    );
    assert!(resident
        .current_binding_in(public, "producerValue")
        .is_none());
    assert!(resident.current_binding_in(public, "producerFn").is_none());
    assert!(!resident
        .prepared_retained()
        .iter()
        .any(
            |(identity, _)| identity.module == value_owner.1.module_name()
                || identity.module == callable_owner.1.module_name()
        ));
    assert_eq!(resident.parked_count(), 0);
}

/// Structural negative only: correct shape metadata is taken from actual
/// compiler output. No product certification or machine installation is minted
/// here. The valid link control must pass before its generation is changed.
fn assert_authentic_retained_link_rejects_wrong_generation(targets: &[Arc<PreparedProgram>]) {
    use tidepool_repr::execution_schema::{link_program, ImportedValue, LinkError, MachineImports};
    let mut tested = false;
    for target in targets {
        let Some(retained) = target
            .globals()
            .iter()
            .find(|global| global.required_generation.is_some())
        else {
            continue;
        };
        let mut imports = MachineImports::default();
        for global in target.globals() {
            imports.values.insert(
                global.identity.clone(),
                ImportedValue {
                    identity: global.identity.clone(),
                    rep: global.rep,
                    entry_signature: global
                        .entry_signature
                        .map(|signature| target.signatures()[signature.0 as usize].clone()),
                    evaluated: global.required_evaluated,
                    generation: global.required_generation.unwrap_or(0),
                },
            );
        }
        assert!(link_program(target.as_ref().clone(), &imports).is_ok());
        let correct = imports.clone();
        let imported = imports.values.get_mut(&retained.identity).unwrap();
        imported.generation = imported.generation.checked_add(1).unwrap();
        assert_eq!(
            link_program(target.as_ref().clone(), &imports).unwrap_err(),
            LinkError::ImportContract(retained.identity.clone())
        );
        let mut missing = correct;
        missing.values.remove(&retained.identity);
        assert_eq!(
            link_program(target.as_ref().clone(), &missing).unwrap_err(),
            LinkError::MissingImport(retained.identity.clone())
        );
        tested = true;
    }
    assert!(
        tested,
        "production consumer must retain a generation-stamped global import"
    );
}
